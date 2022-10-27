use std::{io::Write, ops::Deref, sync::Arc};

use anyhow::Result;
use async_recursion::async_recursion;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sourcemap::SourceMap as CrateMap;
use turbo_tasks::{primitives::BytesVc, TryJoinIterExt};

use crate::source_pos::SourcePos;

#[turbo_tasks::value_trait]
pub trait GenerateSourceMap {
    /// Generates a usable source map, capable of both tracing and stringifying.
    fn generate_source_map(&self) -> SourceMapVc;
}

#[turbo_tasks::value]
pub enum SourceMap {
    Regular(#[turbo_tasks(trace_ignore)] RegularSourceMap),
    Sectioned(#[turbo_tasks(trace_ignore)] SectionedSourceMap),
}

impl SourceMap {
    #[async_recursion]
    async fn encode<W: Write + Send>(&self, w: &mut W) -> Result<()> {
        match self {
            SourceMap::Regular(r) => r.0.to_writer(w)?,
            SourceMap::Sectioned(s) => {
                write!(
                    w,
                    r#"{{
  "version": 3,
  "sections": ["#
                )?;

                let sections = s
                    .sections
                    .iter()
                    .map(async move |s| Ok((s.offset, s.map.await?)))
                    .try_join()
                    .await?;

                let mut first_section = true;
                for (offset, map) in sections {
                    if !first_section {
                        write!(w, ",")?;
                    }
                    first_section = false;

                    write!(
                        w,
                        r#"
    {{"offset": {{"line": {}, "column": {}}}, "map": "#,
                        offset.line, offset.column,
                    )?;

                    map.encode(w).await?;
                    write!(w, r#"}}"#)?;
                }

                write!(
                    w,
                    r#"]
}}"#
                )?;
            }
        }
        Ok(())
    }
}

impl SourceMapVc {
    pub fn new_regular(map: CrateMap) -> Self {
        SourceMap::Regular(RegularSourceMap::new(map)).cell()
    }

    pub fn new_sectioned(sections: Vec<SourceMapSection>) -> Self {
        SourceMap::Sectioned(SectionedSourceMap::new(sections)).cell()
    }
}

#[turbo_tasks::value_impl]
impl SourceMapVc {
    #[turbo_tasks::function]
    pub async fn to_bytes(self) -> Result<BytesVc> {
        let mut bytes = vec![];
        self.await?.encode(&mut bytes).await?;
        Ok(BytesVc::cell(bytes))
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RegularSourceMap(Arc<CrateMapWrapper>);

impl RegularSourceMap {
    fn new(map: CrateMap) -> Self {
        RegularSourceMap(Arc::new(CrateMapWrapper(map)))
    }
}

impl Deref for RegularSourceMap {
    type Target = Arc<CrateMapWrapper>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Eq for RegularSourceMap {}
impl PartialEq for RegularSourceMap {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Wraps the CrateMap struct so that it can be cached in a Vc.
///
/// CrateMap contains a raw pointer, which isn't Send, which is required to
/// cache in a Vc. So, we have wrap it in 4 layers of cruft to do it. We don't
/// actually use the pointer, because we don't perform sources content lookup,
/// so it's fine.
#[derive(Debug)]
pub struct CrateMapWrapper(sourcemap::SourceMap);
unsafe impl Send for CrateMapWrapper {}
unsafe impl Sync for CrateMapWrapper {}

impl Deref for CrateMapWrapper {
    type Target = sourcemap::SourceMap;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Serialize for CrateMapWrapper {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::Error;
        let mut bytes = vec![];
        self.0.to_writer(&mut bytes).map_err(Error::custom)?;
        serializer.serialize_bytes(bytes.as_slice())
    }
}

impl<'de> Deserialize<'de> for CrateMapWrapper {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<CrateMapWrapper, D::Error> {
        use serde::de::Error;
        let bytes = <&[u8]>::deserialize(deserializer)?;
        let map = CrateMap::from_slice(bytes).map_err(Error::custom)?;
        Ok(CrateMapWrapper(map))
    }
}

#[derive(Eq, PartialEq, Debug, Serialize, Deserialize)]
pub struct SectionedSourceMap {
    sections: Vec<SourceMapSection>,
}

impl SectionedSourceMap {
    pub fn new(sections: Vec<SourceMapSection>) -> Self {
        Self { sections }
    }
}

#[derive(Eq, PartialEq, Debug, Serialize, Deserialize)]
pub struct SourceMapSection {
    offset: SourcePos,
    map: SourceMapVc,
}

impl SourceMapSection {
    pub fn new(offset: SourcePos, map: SourceMapVc) -> Self {
        Self { offset, map }
    }
}
