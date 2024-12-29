mod errors;

pub use errors::CogError;

use std::collections::HashMap;
use std::fs::File;
use std::path::Path;
use std::vec;
use std::{fmt::Debug, path::PathBuf};

use std::io::BufWriter;
use tiff::decoder::{ChunkType, Decoder, DecodingResult};
use tiff::tags::Tag::{self, GdalNodata};

use async_trait::async_trait;
use martin_tile_utils::{Format, TileCoord, TileInfo};
use serde::{Deserialize, Serialize};
use tilejson::{tilejson, TileJSON};
use url::Url;

use crate::file_config::FileError;
use crate::{
    config::UnrecognizedValues,
    file_config::{ConfigExtras, FileResult, SourceConfigExtras},
    MartinResult, Source, TileData, UrlQuery,
};

#[derive(Clone, Debug)]
pub struct CogSource {
    id: String,
    path: PathBuf,
    meta: Meta,
    tilejson: TileJSON,
    tileinfo: TileInfo,
}

#[derive(Clone, Debug)]
struct Meta {
    min_zoom: u8,
    max_zoom: u8,
    zoom_and_ifd: HashMap<u8, usize>,
    zoom_and_tile_across_down: HashMap<u8, (u32, u32)>,
    nodata: Option<f64>,
    origin: [f64; 3],  // [x, y, z] coordinates
    extent: [f64; 4],  // [minx, miny, maxx, maxy] bounds
    resolutions: HashMap<u8, [f64; 3]>,  // Map of zoom level to [resX, resY, resZ]
}

#[async_trait]
impl Source for CogSource {
    fn get_id(&self) -> &str {
        &self.id
    }

    fn get_tilejson(&self) -> &TileJSON {
        &self.tilejson
    }

    fn get_tile_info(&self) -> TileInfo {
        self.tileinfo
    }

    fn clone_source(&self) -> Box<dyn Source> {
        Box::new(self.clone())
    }

    #[allow(clippy::cast_sign_loss)]
    #[allow(clippy::cast_possible_truncation)]
    #[allow(clippy::too_many_lines)]
    async fn get_tile(
        &self,
        xyz: TileCoord,
        _url_query: Option<&UrlQuery>,
    ) -> MartinResult<TileData> {
        let tif_file =
            File::open(&self.path).map_err(|e| FileError::IoError(e, self.path.clone()))?;
        let mut decoder =
            Decoder::new(tif_file).map_err(|e| CogError::InvalidTiffFile(e, self.path.clone()))?;
        decoder = decoder.with_limits(tiff::decoder::Limits::unlimited());

        let ifd = self.meta.zoom_and_ifd.get(&(xyz.z)).ok_or_else(|| {
            CogError::ZoomOutOfRange(
                xyz.z,
                self.path.clone(),
                self.meta.min_zoom,
                self.meta.max_zoom,
            )
        })?;

        decoder
            .seek_to_image(*ifd)
            .map_err(|e| CogError::IfdSeekFailed(e, *ifd, self.path.clone()))?;

        let tiles_across = self
            .meta
            .zoom_and_tile_across_down
            .get(&(xyz.z))
            .ok_or_else(|| {
                CogError::ZoomOutOfRange(
                    xyz.z,
                    self.path.clone(),
                    self.meta.min_zoom,
                    self.meta.max_zoom,
                )
            })?
            .0;
        let tile_idx = xyz.y * tiles_across + xyz.x;
        let decode_result = decoder
            .read_chunk(tile_idx)
            .map_err(|e| CogError::ReadChunkFailed(e, tile_idx, *ifd, self.path.clone()))?;
        let color_type = decoder
            .colortype()
            .map_err(|e| CogError::InvalidTiffFile(e, self.path.clone()))?;

        let tile_width = decoder.chunk_dimensions().0;
        let tile_height = decoder.chunk_dimensions().1;
        let (data_width, data_height) = decoder.chunk_data_dimensions(tile_idx);

        //do more research on the not u8 case, is this the right way to do it?
        let png_file_bytes = match (decode_result, color_type) {
            (DecodingResult::U8(vec), tiff::ColorType::RGB(_)) => rgb_to_png(
                vec,
                (tile_width, tile_height),
                (data_width, data_height),
                3,
                self.meta.nodata.map(|v| v as u8),
                &self.path,
            ),
            (DecodingResult::U8(vec), tiff::ColorType::RGBA(_)) => rgb_to_png(
                vec,
                (tile_width, tile_height),
                (data_width, data_height),
                4,
                self.meta.nodata.map(|v| v as u8),
                &self.path,
            ),
            (_, _) => Err(CogError::NotSupportedColorTypeAndBitDepth(
                color_type,
                self.path.clone(),
            )),
            // do others in next PRs, a lot of disscussion would be needed
        }?;
        Ok(png_file_bytes)
    }
}

fn rgb_to_png(
    vec: Vec<u8>,
    (tile_width, tile_height): (u32, u32),
    (data_width, data_height): (u32, u32),
    chunk_components_count: u32,
    nodata: Option<u8>,
    path: &Path,
) -> Result<Vec<u8>, CogError> {
    let is_padded = data_width != tile_width;
    let need_add_alpha = chunk_components_count != 4;

    let pixels = if nodata.is_some() || need_add_alpha || is_padded {
        let mut result_vec = vec![0; (tile_width * tile_height * 4) as usize];
        for row in 0..data_height {
            'outer: for col in 0..data_width {
                let idx_chunk =
                    row * data_width * chunk_components_count + col * chunk_components_count;
                let idx_result = row * tile_width * 4 + col * 4;
                for component_idx in 0..chunk_components_count {
                    if nodata.eq(&Some(vec[(idx_chunk + component_idx) as usize])) {
                        //This pixel is nodata, just make it transparent and skip it then
                        let alpha_idx = (idx_result + 3) as usize;
                        result_vec[alpha_idx] = 0;
                        continue 'outer;
                    }
                    result_vec[(idx_result + component_idx) as usize] =
                        vec[(idx_chunk + component_idx) as usize];
                }
                if need_add_alpha {
                    let alpha_idx = (idx_result + 3) as usize;
                    result_vec[alpha_idx] = 255;
                }
            }
        }
        result_vec
    } else {
        vec
    };
    let mut result_file_buffer = Vec::new();
    {
        let mut encoder = png::Encoder::new(
            BufWriter::new(&mut result_file_buffer),
            tile_width,
            tile_height,
        );
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|e| CogError::WritePngHeaderFailed(path.to_path_buf(), e))?;
        writer
            .write_image_data(&pixels)
            .map_err(|e| CogError::WriteToPngFailed(path.to_path_buf(), e))?;
    }
    Ok(result_file_buffer)
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CogConfig {
    #[serde(flatten)]
    pub unrecognized: UnrecognizedValues,
}

impl ConfigExtras for CogConfig {
    fn get_unrecognized(&self) -> &UnrecognizedValues {
        &self.unrecognized
    }
}

impl SourceConfigExtras for CogConfig {
    async fn new_sources(&self, id: String, path: PathBuf) -> FileResult<Box<dyn Source>> {
        let tileinfo = TileInfo::new(Format::Png, martin_tile_utils::Encoding::Uncompressed);
        let meta = get_meta(&path)?;
        let tilejson = tilejson! {
            tiles: vec![],
            minzoom: meta.min_zoom,
            maxzoom: meta.max_zoom
        };
        Ok(Box::new(CogSource {
            id,
            path,
            meta,
            tilejson,
            tileinfo,
        }))
    }

    #[allow(clippy::no_effect_underscore_binding)]
    async fn new_sources_url(&self, _id: String, _url: Url) -> FileResult<Box<dyn Source>> {
        unreachable!()
    }

    fn parse_urls() -> bool {
        false
    }
}

fn get_meta(path: &PathBuf) -> Result<Meta, FileError> {
    let tif_file = File::open(path).map_err(|e| FileError::IoError(e, path.clone()))?;
    let mut decoder = Decoder::new(tif_file)
        .map_err(|e| CogError::InvalidTiffFile(e, path.clone()))?
        .with_limits(tiff::decoder::Limits::unlimited());

    let chunk_type = decoder.get_chunk_type();

    if chunk_type != ChunkType::Tile {
        Err(CogError::NotSupportedChunkType(path.clone()))?;
    }

    let color_type = decoder
        .colortype()
        .map_err(|e| CogError::InvalidTiffFile(e, path.clone()))?;

    if !matches!(
        color_type,
        tiff::ColorType::RGB(8) | tiff::ColorType::RGBA(8)
    ) {
        Err(CogError::NotSupportedColorTypeAndBitDepth(
            color_type,
            path.clone(),
        ))?;
    }

    decoder
        .get_tag_unsigned(Tag::PlanarConfiguration)
        .map_err(|e| {
            CogError::TagsNotFound(e, vec![Tag::PlanarConfiguration.to_u16()], 0, path.clone())
        })
        .and_then(|config| {
            if config == 1 {
                Ok(())
            } else {
                Err(CogError::PlanarConfigurationNotSupported(
                    path.clone(),
                    0,
                    config,
                ))
            }
        })?;

    let tag = decoder.get_tag_ascii_string(GdalNodata);
    let nodata: Option<f64> = if let Ok(nodata_tag) = tag {
        nodata_tag.parse().ok()
    } else {
        None
    };
    let images_ifd = get_images_ifd(&mut decoder);

    let mut zoom_and_ifd: HashMap<u8, usize> = HashMap::new();
    let mut zoom_and_tile_across_down: HashMap<u8, (u32, u32)> = HashMap::new();

    for image_ifd in &images_ifd {
        decoder
            .seek_to_image(*image_ifd)
            .map_err(|e| CogError::IfdSeekFailed(e, *image_ifd, path.clone()))?;

        let zoom = u8::try_from(images_ifd.len() - (image_ifd + 1))
            .map_err(|_| CogError::TooManyImages(path.clone()))?;

        let (tiles_across, tiles_down) = get_grid_dims(&mut decoder, path, *image_ifd)?;

        zoom_and_ifd.insert(zoom, *image_ifd);
        zoom_and_tile_across_down.insert(zoom, (tiles_across, tiles_down));
    }

    let min_zoom = zoom_and_ifd
        .keys()
        .min()
        .ok_or_else(|| CogError::NoImagesFound(path.clone()))?;

    let max_zoom = zoom_and_ifd
        .keys()
        .max()
        .ok_or_else(|| CogError::NoImagesFound(path.clone()))?;

    let (width, height) = decoder.dimensions().map_err(|e| {
        CogError::TagsNotFound(
            e,
            vec![Tag::ImageWidth.to_u16(), Tag::ImageLength.to_u16()],
            0,
            path.to_path_buf(),
        )
    })?;

    let model_transformation = decoder.get_tag_f64_vec(Tag::ModelTransformationTag).ok();
    let model_tiepoint = decoder.get_tag_f64_vec(Tag::ModelTiepointTag).ok();
    let pixel_scale = decoder.get_tag_f64_vec(Tag::ModelPixelScaleTag).ok();

    // Get origin and extent
    let origin = get_origin(
        model_transformation.as_deref(),
        model_tiepoint.as_deref(),
        path,
    )?;

    let extent = get_extent(
        model_transformation.clone(),
        model_tiepoint.clone(),
        pixel_scale.clone(),
        width,
        height,
        path.clone(),
    )?;

    // Calculate resolutions for each zoom level
    let mut resolutions = HashMap::new();
    for image_ifd in &images_ifd {
        decoder.seek_to_image(*image_ifd)
            .map_err(|e| CogError::IfdSeekFailed(e, *image_ifd, path.clone()))?;

        let zoom = u8::try_from(images_ifd.len() - (image_ifd + 1))
            .map_err(|_| CogError::TooManyImages(path.clone()))?;

        let (img_width, img_height) = decoder.dimensions()
            .map_err(|e| CogError::TagsNotFound(
                e,
                vec![Tag::ImageWidth.to_u16(), Tag::ImageLength.to_u16()],
                *image_ifd,
                path.to_path_buf(),
            ))?;

        let resolution = get_resolution(
            pixel_scale.as_deref(),
            model_transformation.as_deref(),
            None,
            img_width,
            img_height,
            path,
        )?;

        resolutions.insert(zoom, resolution);
    }

    Ok(Meta {
        min_zoom: *min_zoom,
        max_zoom: *max_zoom,
        zoom_and_ifd,
        zoom_and_tile_across_down,
        nodata,
        origin,
        extent,
        resolutions,
    })
}

/// Calculate the extent [minx, miny, maxx, maxy] of a GeoTIFF image
fn get_extent(
    model_transformation: Option<Vec<f64>>,
    model_tiepoint: Option<Vec<f64>>,
    pixel_scale: Option<Vec<f64>>,
    width: u32,
    height: u32,
    path: PathBuf,
) -> Result<[f64; 4], CogError> {
    match (model_transformation, model_tiepoint, pixel_scale) {
        (Some(transform), _, _) => get_extent_from_transform(&transform, width, height),
        (None, Some(tiepoint), Some(pixel_scale)) => {
            get_extent_from_tiepoint(&tiepoint, &pixel_scale, width, height)
        }
        _ => Err(CogError::MissingGeospatialInfo(path)),
    }
}

/// Calculate the extent [minx, miny, maxx, maxy] using model transformation matrix
fn get_extent_from_transform(
    transform: &[f64],
    width: u32,
    height: u32,
) -> Result<[f64; 4], CogError> {
    // ModelTransformationTag should have at least 12 values for a 3x4 matrix
    if transform.len() < 12 {
        return Err(CogError::InvalidModelTransformation(transform.len()));
    }

    let corners = [
        (0.0, 0.0),
        (0.0, height as f64),
        (width as f64, 0.0),
        (width as f64, height as f64),
    ];

    let mut xs = Vec::with_capacity(4);
    let mut ys = Vec::with_capacity(4);

    // Apply transformation matrix to each corner
    for (i, j) in corners {
        let x = transform[3] + (transform[0] * i) + (transform[1] * j);
        let y = transform[7] + (transform[4] * i) + (transform[5] * j);
        xs.push(x);
        ys.push(y);
    }

    Ok([
        *xs.iter().min_by(|a, b| a.total_cmp(b)).unwrap(),
        *ys.iter().min_by(|a, b| a.total_cmp(b)).unwrap(),
        *xs.iter().max_by(|a, b| a.total_cmp(b)).unwrap(),
        *ys.iter().max_by(|a, b| a.total_cmp(b)).unwrap(),
    ])
}

/// Calculate the extent [minx, miny, maxx, maxy] using model tiepoint and pixel scale
fn get_extent_from_tiepoint(
    tiepoint: &[f64],
    pixel_scale: &[f64],
    width: u32,
    height: u32,
) -> Result<[f64; 4], CogError> {
    // ModelTiepointTag should have at least 6 values (I,J,K,X,Y,Z)
    if tiepoint.len() < 6 {
        return Err(CogError::InvalidModelTiepoint(tiepoint.len()));
    }

    // ModelPixelScaleTag should have 3 values (ScaleX, ScaleY, ScaleZ)
    if pixel_scale.len() < 3 {
        return Err(CogError::InvalidModelPixelScale(pixel_scale.len()));
    }

    let x1 = tiepoint[3]; // Origin X
    let y1 = tiepoint[4]; // Origin Y

    // Calculate max extent using resolution/pixel scale
    let x2 = x1 + (pixel_scale[0] * width as f64);
    let y2 = y1 + (pixel_scale[1] * height as f64);

    Ok([x1.min(x2), y1.min(y2), x1.max(x2), y1.max(y2)])
}

/// Get the origin [x, y, z] coordinates from either ModelTransformation or ModelTiepoint
///
/// The origin is determined in the following order:
/// 1. If ModelTransformation is present, use [transform[3], transform[7], transform[11]]
/// 2. If ModelTiepoint is present, use [tiepoint[3], tiepoint[4], tiepoint[5]]
/// 3. Otherwise return an error
///
/// @param model_transformation Optional ModelTransformation matrix (3x4)
/// @param model_tiepoint Optional ModelTiepoint coordinates
/// @param path Path to the TIFF file for error reporting
/// @returns Result with [x, y, z] origin coordinates
fn get_origin(
    model_transformation: Option<&[f64]>,
    model_tiepoint: Option<&[f64]>,
    path: &PathBuf,
) -> Result<[f64; 3], CogError> {
    match (model_transformation, model_tiepoint) {
        (Some(transform), _) => {
            if transform.len() < 12 {
                return Err(CogError::InvalidModelTransformation(transform.len()));
            }
            Ok([transform[3], transform[7], transform[11]])
        }
        (None, Some(tiepoint)) => {
            if tiepoint.len() < 6 {
                return Err(CogError::InvalidModelTiepoint(tiepoint.len()));
            }
            Ok([tiepoint[3], tiepoint[4], tiepoint[5]])
        }
        (None, None) => Err(CogError::CannotDetermineOrigin(path.clone())),
    }
}

// ...existing code...

/// Get the resolution [x, y, z] from either ModelTransformation or ModelPixelScale
///
/// The resolution is determined in the following order:
/// 1. If ModelPixelScale is present, use [scaleX, -scaleY, scaleZ]
/// 2. If ModelTransformation is present:
///    - If matrix is axis-aligned (no rotation), use [M00, -M11, M22]
///    - Otherwise calculate magnitude of transformation vectors
/// 3. If reference image is provided, calculate relative resolution
/// 4. Otherwise return an error
///
/// @param model_pixel_scale Optional ModelPixelScale values
/// @param model_transformation Optional ModelTransformation matrix (3x4)
/// @param reference Optional reference image dimensions and resolution
/// @param width Current image width
/// @param height Current image height
/// @param path Path to the TIFF file for error reporting
/// @returns Result with [resX, resY, resZ] resolution values
#[derive(Debug)]
pub struct ReferenceImage {
    pub width: u32,
    pub height: u32,
    pub resolution: [f64; 3],
}

fn get_resolution(
    model_pixel_scale: Option<&[f64]>,
    model_transformation: Option<&[f64]>,
    reference: Option<&ReferenceImage>,
    width: u32,
    height: u32,
    path: &PathBuf,
) -> Result<[f64; 3], CogError> {
    if let Some(pixel_scale) = model_pixel_scale {
        if pixel_scale.len() < 3 {
            return Err(CogError::InvalidModelPixelScale(pixel_scale.len()));
        }
        return Ok([pixel_scale[0], -pixel_scale[1], pixel_scale[2]]);
    }

    if let Some(transform) = model_transformation {
        if transform.len() < 12 {
            return Err(CogError::InvalidModelTransformation(transform.len()));
        }

        // Check if matrix is axis-aligned (no rotation)
        if transform[1] == 0.0 && transform[4] == 0.0 {
            return Ok([transform[0], -transform[5], transform[10]]);
        }

        // Calculate magnitude of transformation vectors for rotated case
        let res_x = (transform[0] * transform[0] + transform[4] * transform[4]).sqrt();
        let res_y = -((transform[1] * transform[1] + transform[5] * transform[5]).sqrt());
        let res_z = transform[10];

        return Ok([res_x, res_y, res_z]);
    }

    if let Some(ref_img) = reference {
        if ref_img.width == 0 || ref_img.height == 0 {
            return Err(CogError::InvalidReferenceImageDimensions(path.clone()));
        }

        // Calculate relative resolution based on reference image
        let res_x = ref_img.resolution[0] * (ref_img.width as f64) / (width as f64);
        let res_y = ref_img.resolution[1] * (ref_img.height as f64) / (height as f64);
        let res_z = ref_img.resolution[2] * (ref_img.width as f64) / (width as f64);

        return Ok([res_x, res_y, res_z]);
    }

    Err(CogError::CannotDetermineResolution(path.clone()))
}

fn get_grid_dims(
    decoder: &mut Decoder<File>,
    path: &Path,
    image_ifd: usize,
) -> Result<(u32, u32), FileError> {
    let (tile_width, tile_height) = (decoder.chunk_dimensions().0, decoder.chunk_dimensions().1);
    let (image_width, image_length) = get_image_dims(decoder, path, image_ifd)?;
    let tiles_across = image_width.div_ceil(tile_width);
    let tiles_down = image_length.div_ceil(tile_height);

    Ok((tiles_across, tiles_down))
}

fn get_image_dims(
    decoder: &mut Decoder<File>,
    path: &Path,
    image_ifd: usize,
) -> Result<(u32, u32), FileError> {
    let (image_width, image_length) = decoder.dimensions().map_err(|e| {
        CogError::TagsNotFound(
            e,
            vec![Tag::ImageWidth.to_u16(), Tag::ImageLength.to_u16()],
            image_ifd,
            path.to_path_buf(),
        )
    })?;

    Ok((image_width, image_length))
}

fn get_images_ifd(decoder: &mut Decoder<File>) -> Vec<usize> {
    let mut res = vec![];
    let mut ifd_idx = 0;
    loop {
        let is_image = decoder
            .get_tag_u32(Tag::NewSubfileType)
            .map_or_else(|_| true, |v| v & 4 != 4);
        if is_image {
            //todo We should not ignore mask in the next PRs
            res.push(ifd_idx);
        }

        ifd_idx += 1;

        let next_res = decoder.seek_to_image(ifd_idx);
        if next_res.is_err() {
            break;
        }
    }
    res
}
