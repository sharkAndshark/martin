mod errors;

pub use errors::CogError;
use log::warn;
use regex::Regex;

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

impl CogSource {
    pub fn new(id: String, path: PathBuf) -> FileResult<Self> {
        let tileinfo = TileInfo::new(Format::Png, martin_tile_utils::Encoding::Uncompressed);
        let meta = get_meta(&path)?;
        let tilejson = tilejson! {
            tiles: vec![],
            minzoom: meta.min_zoom,
            maxzoom: meta.max_zoom
        };
        Ok(CogSource {
            id,
            path,
            meta,
            tilejson,
            tileinfo,
        })
    }
}

#[derive(Clone, Debug)]
struct Meta {
    min_zoom: u8,
    max_zoom: u8,
    zoom_and_ifd: HashMap<u8, usize>,
    zoom_and_tile_across_down: HashMap<u8, (u32, u32)>,
    nodata: Option<f64>,
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

        let (tile_width, tile_height) = decoder.chunk_dimensions();
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
        let cog = CogSource::new(id, path)?;
        Ok(Box::new(cog))
    }

    #[allow(clippy::no_effect_underscore_binding)]
    async fn new_sources_url(&self, _id: String, _url: Url) -> FileResult<Box<dyn Source>> {
        unreachable!()
    }

    fn parse_urls() -> bool {
        false
    }
}
fn verify_requirments(decoder: &mut Decoder<File>, path: &Path) -> Result<(), CogError> {
    let chunk_type = decoder.get_chunk_type();
    // see the requirement 2 in https://docs.ogc.org/is/21-026/21-026.html#_tiles
    if chunk_type != ChunkType::Tile {
        Err(CogError::NotSupportedChunkType(path.to_path_buf()))?;
    }

    // see https://docs.ogc.org/is/21-026/21-026.html#_planar_configuration_considerations and https://www.verypdf.com/document/tiff6/pg_0038.htm
    // we might support planar configuration 2 in the future
    decoder
        .get_tag_unsigned(Tag::PlanarConfiguration)
        .map_err(|e| {
            CogError::TagsNotFound(
                e,
                vec![Tag::PlanarConfiguration.to_u16()],
                0,
                path.to_path_buf(),
            )
        })
        .and_then(|config| {
            if config == 1 {
                Ok(())
            } else {
                Err(CogError::PlanarConfigurationNotSupported(
                    path.to_path_buf(),
                    0,
                    config,
                ))
            }
        })?;

    let color_type = decoder
        .colortype()
        .map_err(|e| CogError::InvalidTiffFile(e, path.to_path_buf()))?;

    if !matches!(
        color_type,
        tiff::ColorType::RGB(8) | tiff::ColorType::RGBA(8)
    ) {
        Err(CogError::NotSupportedColorTypeAndBitDepth(
            color_type,
            path.to_path_buf(),
        ))?;
    };
    Ok(())
}

#[allow(clippy::cast_possible_truncation)]
fn get_meta(path: &PathBuf) -> Result<Meta, FileError> {
    let tif_file = File::open(path).map_err(|e| FileError::IoError(e, path.clone()))?;
    let mut decoder = Decoder::new(tif_file)
        .map_err(|e| CogError::InvalidTiffFile(e, path.clone()))?
        .with_limits(tiff::decoder::Limits::unlimited());

    verify_requirments(&mut decoder, path)?;
    let mut zoom_and_ifd: HashMap<u8, usize> = HashMap::new();
    let mut zoom_and_tile_across_down: HashMap<u8, (u32, u32)> = HashMap::new();

    let nodata: Option<f64> = if let Ok(no_data) = decoder.get_tag_ascii_string(GdalNodata) {
        no_data.parse().ok()
    } else {
        None
    };

    let images_ifd = get_images_ifd(&mut decoder, path);

    for (idx, image_ifd) in images_ifd.iter().enumerate() {
        decoder
            .seek_to_image(*image_ifd)
            .map_err(|e| CogError::IfdSeekFailed(e, *image_ifd, path.clone()))?;

        let zoom = u8::try_from(images_ifd.len() - (idx + 1))
            .map_err(|_| CogError::TooManyImages(path.clone()))?;

        let (tiles_across, tiles_down) = get_grid_dims(&mut decoder, path, *image_ifd)?;

        zoom_and_ifd.insert(zoom, *image_ifd);
        zoom_and_tile_across_down.insert(zoom, (tiles_across, tiles_down));
    }

    if images_ifd.is_empty() {
        Err(CogError::NoImagesFound(path.clone()))?;
    }

    Ok(Meta {
        min_zoom: 0,
        max_zoom: images_ifd.len() as u8 - 1,
        zoom_and_ifd,
        zoom_and_tile_across_down,
        nodata,
    })
}

fn google_stuffs(
    min_zoom: u8,
    max_zoom: u8,
    decoder: &mut Decoder<File>,
    chunk_size: u32,
    path: &PathBuf,
) -> Result<(), CogError> {
    let gdal_metadata = decoder
        .get_tag_ascii_string(Tag::Unknown(42112))
        .map_err(|e| CogError::TagsNotFound(e, vec![42112], 0, PathBuf::new()))?;

    let mut tiling_schema_name = None;
    let mut zoom_level: Option<u8> = None;

    let re_name = Regex::new(r#"<Item name="NAME" domain="TILING_SCHEME">([^<]+)</Item>"#).unwrap();
    let re_zoom =
        Regex::new(r#"<Item name="ZOOM_LEVEL" domain="TILING_SCHEME">([^<]+)</Item>"#).unwrap();

    if let Some(caps) = re_name.captures(&gdal_metadata) {
        tiling_schema_name = Some(caps[1].to_string());
    }

    if let Some(caps) = re_zoom.captures(&gdal_metadata) {
        zoom_level = caps[1].parse().ok();
    }

    let google_compatible_max_zoom =
        if tiling_schema_name == Some("GoogleMapsCompatible".to_string()) {
            zoom_level
        } else {
            None
        };
    let google_compatible_min_zoom =
        google_compatible_max_zoom.map(|google_max_zoom| google_max_zoom - max_zoom + min_zoom);
    // google zoom to actual zoom_level
    let zoom_mapping = |zoom: u8| -> Option<u8> {
        let result = if let Some(google_max) = google_compatible_max_zoom {
            Some(max_zoom - google_max + zoom)
        } else {
            None
        };
        if result.is_some_and(|v| v < min_zoom || v > max_zoom) {
            None
        } else {
            result
        }
    };

    let model_transformation = decoder.get_tag_f64_vec(Tag::ModelTransformationTag).ok();
    let model_tiepoint = decoder.get_tag_f64_vec(Tag::ModelTiepointTag).ok();
    let pixel_scale = decoder.get_tag_f64_vec(Tag::ModelPixelScaleTag).ok();

    let mut first_xy = HashMap::new();
    for google_z in google_compatible_min_zoom.unwrap()..google_compatible_max_zoom.unwrap()
    {
        let actual_zoom = zoom_mapping(google_z).ok_or_else(|| {
            CogError::ZoomOutOfRange(
                google_z,
                path.clone(),
                google_compatible_min_zoom.unwrap(),
                google_compatible_max_zoom.unwrap(),
            )
        })?;
        let chunk_size_current = chunk_size * 2_u32.pow(max_zoom as u32 - actual_zoom as u32);
        let first_tile_center = get_first_tile_center_coords(
            model_transformation.as_deref(),
            model_tiepoint.as_deref(),
            pixel_scale.as_deref(),
            chunk_size_current,
            path.clone(),
        )?;
        let tile_idx = get_tile_coords(first_tile_center.0, first_tile_center.1, google_z as u32);
        first_xy.insert(actual_zoom, tile_idx);
    }
    todo!()
}

pub fn get_first_tile_center_coords(
    model_transformation: Option<&[f64]>,
    model_tiepoint: Option<&[f64]>,
    pixel_scale: Option<&[f64]>,
    tile_size: u32,
    path: PathBuf,
) -> Result<(f64, f64), CogError> {
    let tile_size = tile_size as f64;
    let (x, y) = if let Some(transform) = model_transformation {
        // Using model transformation
        let center_x = transform[0] + (tile_size / 2.0) * transform[1];
        let center_y = transform[3] + (tile_size / 2.0) * transform[5];
        (center_x, center_y)
    } else if let (Some(tiepoint), Some(scale)) = (model_tiepoint, pixel_scale) {
        // Using tiepoint and pixel scale
        let center_x = tiepoint[3] + (tile_size / 2.0) * scale[0];
        let center_y = tiepoint[4] - (tile_size / 2.0) * scale[1];
        (center_x, center_y)
    } else {
        //todo help me generate error
        return Err(CogError::MissingGeospatialInfo(path));
    };

    Ok((x, y))
}

fn get_tile_coords(coord_x: f64, coord_y: f64, zoom: u32) -> (u32, u32) {
    const EARTH_RADIUS_PI: f64 = 20037508.34;
    let num_tiles = 2_u32.pow(zoom) as f64;
    let tile_size = (2.0 * EARTH_RADIUS_PI) / num_tiles;

    let x_tile = ((coord_x + EARTH_RADIUS_PI) / tile_size).floor() as u32;
    let y_tile = ((EARTH_RADIUS_PI - coord_y) / tile_size).floor() as u32;

    (x_tile, y_tile)
}

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

#[derive(Debug)]
struct ReferenceImage {
    width: u32,
    height: u32,
    resolution: [f64; 3],
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
#[allow(clippy::cast_lossless)]
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
#[allow(clippy::cast_lossless)]
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

fn get_images_ifd(decoder: &mut Decoder<File>, path: &Path) -> Vec<usize> {
    let mut res = vec![];
    let mut ifd_idx = 0;
    loop {
        let is_image = decoder
            .get_tag_u32(Tag::NewSubfileType)
            .map_or_else(|_| true, |v| v & 4 != 4); // see https://www.verypdf.com/document/tiff6/pg_0036.htm
        if is_image {
            //todo We should not ignore mask in the next PRs
            res.push(ifd_idx);
        } else {
            warn!(
                "A subfile of {} is ignored in the tiff file as Martin currently does not support mask subfile in tiff. The ifd number of this subfile is {}",
                path.display(),
                ifd_idx
            );
        }

        ifd_idx += 1;

        let next_res = decoder.seek_to_image(ifd_idx);
        if next_res.is_err() {
            break;
        }
    }
    res
}
