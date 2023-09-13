use std::{path::PathBuf, fmt::Formatter};
use std::fmt::{Debug};
use async_trait::async_trait;
use martin_tile_utils::TileInfo;
use tilejson::TileJSON;
use crate::utils::Error;
use crate::{file_config::FileError, Source, Xyz, source::{UrlQuery, Tile}};

#[derive(Clone)]
pub struct CogSource{
    id: String,
    path: PathBuf,
}

impl CogSource {
    pub async fn new_box(id: String, path: PathBuf) -> Result<Box<dyn Source>, FileError> {
        Ok(Box::new(CogSource::new(id, path).await?))
    }

    async fn new(id: String, path: PathBuf) -> Result<Self, FileError> { 
        todo!()
    }

    fn get_tile(xyz: &Xyz) -> Result<Tile,Error>{
        todo!()
    }
}

impl Debug for CogSource{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "CogSource {{ id: {}, path: {:?} }}", self.id, self.path)
    }
}

#[async_trait]
impl Source for CogSource{
    fn get_tilejson(&self) -> TileJSON {
        todo!()
    }

    fn get_tile_info(&self) -> TileInfo {
        todo!()
    }

    fn clone_source(&self) -> Box<dyn Source>  {
        Box::new(self.clone())
    }

    fn is_valid_zoom(&self,zoom:u8) -> bool {
        todo!()
    }

    fn support_url_query(&self) -> bool {
        false
    }

    async fn get_tile(&self, xyz: &Xyz, query: &Option<UrlQuery>) -> Result<Tile,Error>{
        todo!()
    }
}

#[cfg(test)]
mod tests{
    #[test]
    fn test(){
        let path = "tests/fixtures/files/cog.tif";
        println!("{}",path);
    }
}