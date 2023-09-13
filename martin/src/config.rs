use std::collections::HashMap;
use std::fs::File;
use std::future::Future;
use std::io::prelude::*;
use std::path::Path;
use std::pin::Pin;

use futures::future::try_join_all;
use serde::{Deserialize, Serialize};
use subst::VariableMap;

use crate::cog::CogSource;
use crate::file_config::{resolve_files, FileConfigEnum};
use crate::mbtiles::MbtSource;
use crate::pg::PgConfig;
use crate::pmtiles::PmtSource;
use crate::source::Sources;
use crate::sprites::{resolve_sprites, SpriteSources};
use crate::srv::SrvConfig;
use crate::utils::{IdResolver, OneOrMany, Result};
use crate::Error::{ConfigLoadError, ConfigParseError, NoSources};

pub type UnrecognizedValues = HashMap<String, serde_yaml::Value>;

pub struct AllSources {
    pub sources: Sources,
    pub sprites: SpriteSources,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Config {
    #[serde(flatten)]
    pub srv: SrvConfig,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub postgres: Option<OneOrMany<PgConfig>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub pmtiles: Option<FileConfigEnum>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub mbtiles: Option<FileConfigEnum>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub cogs: Option<FileConfigEnum>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub sprites: Option<FileConfigEnum>,

    #[serde(flatten)]
    pub unrecognized: UnrecognizedValues,
}

impl Config {
    /// Apply defaults to the config, and validate if there is a connection string
    pub fn finalize(&mut self) -> Result<UnrecognizedValues> {
        let mut res = UnrecognizedValues::new();
        copy_unrecognized_config(&mut res, "", &self.unrecognized);

        let mut any = if let Some(pg) = &mut self.postgres {
            for pg in pg.iter_mut() {
                res.extend(pg.finalize()?);
            }
            !pg.is_empty()
        } else {
            false
        };

        any |= if let Some(cfg) = &mut self.pmtiles {
            res.extend(cfg.finalize("pmtiles.")?);
            !cfg.is_empty()
        } else {
            false
        };

        any |= if let Some(cfg) = &mut self.mbtiles {
            res.extend(cfg.finalize("mbtiles.")?);
            !cfg.is_empty()
        } else {
            false
        };

        any |= if let Some(cfg) = &mut self.sprites {
            res.extend(cfg.finalize("sprites.")?);
            !cfg.is_empty()
        } else {
            false
        };

        if any {
            Ok(res)
        } else {
            Err(NoSources)
        }
    }

    pub async fn resolve(&mut self, idr: IdResolver) -> Result<AllSources> {
        let create_pmt_src = &mut PmtSource::new_box;
        let create_mbt_src = &mut MbtSource::new_box;
        let create_cog_src = &mut CogSource::new_box;

        let mut sources: Vec<Pin<Box<dyn Future<Output = Result<Sources>>>>> = Vec::new();
        if let Some(v) = self.postgres.as_mut() {
            for s in v.iter_mut() {
                sources.push(Box::pin(s.resolve(idr.clone())));
            }
        }
        if self.pmtiles.is_some() {
            let val = resolve_files(&mut self.pmtiles, idr.clone(), "pmtiles", create_pmt_src);
            sources.push(Box::pin(val));
        }

        if self.mbtiles.is_some() {
            let val = resolve_files(&mut self.mbtiles, idr.clone(), "mbtiles", create_mbt_src);
            sources.push(Box::pin(val));
        }

        if self.cogs.is_some() {
            let val = resolve_files(&mut self.cogs, idr.clone(), "mbtiles", create_cog_src);
            sources.push(Box::pin(val));
        }

        // Minor in-efficiency:
        // Sources are added to a BTreeMap, then iterated over into a sort structure and convert back to a BTreeMap.
        // Ideally there should be a vector of values, which is then sorted (in-place?) and converted to a BTreeMap.
        Ok(AllSources {
            sources: try_join_all(sources)
                .await?
                .into_iter()
                .fold(Sources::default(), |mut acc, hashmap| {
                    acc.extend(hashmap);
                    acc
                })
                .sort(),
            sprites: resolve_sprites(&mut self.sprites)?,
        })
    }
}

pub fn copy_unrecognized_config(
    result: &mut UnrecognizedValues,
    prefix: &str,
    unrecognized: &UnrecognizedValues,
) {
    result.extend(
        unrecognized
            .iter()
            .map(|(k, v)| (format!("{prefix}{k}"), v.clone())),
    );
}

/// Read config from a file
pub fn read_config<'a, M>(file_name: &Path, env: &'a M) -> Result<Config>
where
    M: VariableMap<'a>,
    M::Value: AsRef<str>,
{
    let mut file = File::open(file_name).map_err(|e| ConfigLoadError(e, file_name.into()))?;
    let mut contents = String::new();
    file.read_to_string(&mut contents)
        .map_err(|e| ConfigLoadError(e, file_name.into()))?;
    parse_config(&contents, env, file_name)
}

pub fn parse_config<'a, M>(contents: &str, env: &'a M, file_name: &Path) -> Result<Config>
where
    M: VariableMap<'a>,
    M::Value: AsRef<str>,
{
    subst::yaml::from_str(contents, env).map_err(|e| ConfigParseError(e, file_name.into()))
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::config::Config;
    use crate::test_utils::FauxEnv;

    pub fn parse_cfg(yaml: &str) -> Config {
        parse_config(yaml, &FauxEnv::default(), Path::new("<test>")).unwrap()
    }

    pub fn assert_config(yaml: &str, expected: &Config) {
        let mut config = parse_cfg(yaml);
        let res = config.finalize().unwrap();
        assert!(res.is_empty(), "unrecognized config: {res:?}");
        assert_eq!(&config, expected);
    }
}
