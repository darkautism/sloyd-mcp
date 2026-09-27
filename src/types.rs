use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

/// Sloyd "Optimize for" presets, mirrored from app.sloyd.ai. A preset supplies the
/// default polycount / texture / topology and may add a pipeline flag (e.g. `-lowpoly`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Preset {
    /// Game dev, 40k faces, 2k texture, auto topology.
    GameHighpoly,
    /// Game dev, low-poly pipeline: 10k faces, 1k texture, triangles.
    #[default]
    GameLowpoly,
    /// Roblox asset, low-poly pipeline: 20k faces, 1k texture, triangles.
    GameRoblox,
    /// Roblox accessory, low-poly pipeline: 4k faces, 1k texture, triangles.
    GameRobloxAccessory,
    /// Roblox character, low-poly pipeline: 10k faces, 1k texture, triangles.
    GameRobloxCharacter,
    /// 3D print (FDM/resin): 100k faces, no texture, triangles.
    PrintStandard,
    /// Multicolor 3D print: 100k faces, 2k texture, triangles.
    PrintMulticolor,
    /// High-detail 3D print: 500k faces, no texture, triangles.
    PrintUltra,
    /// Product/architecture render: 100k faces, 2k texture, triangles.
    VizStandard,
    /// Cinematic render: 500k faces, 4k texture, triangles.
    VizUltra,
    /// Environment map: 100k faces, 2k texture, auto topology.
    VisualizationEnv,
    /// No pipeline flag; 40k faces, 2k texture, auto topology. Tune the rest manually.
    Custom,
}

struct PresetDefaults {
    flag: &'static str,
    polycount: u32,
    texture: Texture,
    topology: Topology,
}

impl Preset {
    fn defaults(self) -> PresetDefaults {
        use {Texture as X, Topology as T};
        let (flag, polycount, texture, topology) = match self {
            Self::GameHighpoly => ("", 40_000, X::R2k, T::Auto),
            Self::GameLowpoly => ("-lowpoly", 10_000, X::R1k, T::Triangles),
            Self::GameRoblox => ("-lowpoly", 20_000, X::R1k, T::Triangles),
            Self::GameRobloxAccessory => ("-lowpoly", 4_000, X::R1k, T::Triangles),
            Self::GameRobloxCharacter => ("-lowpoly", 10_000, X::R1k, T::Triangles),
            Self::PrintStandard => ("-3dprint", 100_000, X::None, T::Triangles),
            Self::PrintMulticolor => ("", 100_000, X::R2k, T::Triangles),
            Self::PrintUltra => ("-3dprint", 500_000, X::None, T::Triangles),
            Self::VizStandard => ("", 100_000, X::R2k, T::Triangles),
            Self::VizUltra => ("", 500_000, X::R4k, T::Triangles),
            Self::VisualizationEnv => ("-envmap", 100_000, X::R2k, T::Auto),
            Self::Custom => ("", 40_000, X::R2k, T::Auto),
        };
        PresetDefaults {
            flag,
            polycount,
            texture,
            topology,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Topology {
    Auto,
    /// Optimized for game engines; maximizes detail at a given polycount.
    Triangles,
    /// Evenly distributed quad faces; best for further editing.
    Quads,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Texture {
    /// Untextured mesh.
    #[serde(rename = "none")]
    None,
    #[serde(rename = "512")]
    R512,
    #[serde(rename = "1k")]
    R1k,
    #[serde(rename = "2k")]
    R2k,
    /// Subscribers only.
    #[serde(rename = "4k")]
    R4k,
}

/// Art style for Text-to-3D (Sloyd `genStyleId`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Style {
    /// No style preset.
    #[default]
    Auto,
    Cartoon,
    CelShadedComic,
    ClayMorphic,
    Color3DPrint,
    CozyMobile,
    DetailedAnime,
    HandpaintedStylized,
    IsometricDiorama,
    PainterlyComic,
    /// Faceted low-poly look.
    Polygonal,
    Realistic,
    RetroCRT90s,
    Roblox,
    Sketch,
    StylizedAnime,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum License {
    /// Only you can use the model (subscribers).
    #[default]
    Private,
    /// Public, Creative Commons Attribution 4.0.
    #[serde(rename = "cc-by-4.0")]
    CcBy4,
}

pub const POLYCOUNTS: &[u32] = &[
    3_000, 4_000, 5_000, 10_000, 20_000, 40_000, 100_000, 200_000, 500_000,
];

/// Fully resolved generation settings: every field is what Sloyd will receive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<PathBuf>,
    pub prompt: String,
    pub preset: Preset,
    pub style: Style,
    pub polycount: u32,
    pub topology: Topology,
    pub texture: Texture,
    pub t_pose: bool,
    pub refine: bool,
    pub license: License,
}

/// Optional overrides layered on top of a preset.
#[derive(Debug, Clone, Default)]
pub struct Overrides {
    pub polycount: Option<u32>,
    pub topology: Option<Topology>,
    pub texture: Option<Texture>,
}

impl GenerateRequest {
    #[allow(clippy::too_many_arguments)]
    pub fn resolve(
        name: Option<String>,
        image: Option<PathBuf>,
        prompt: String,
        preset: Preset,
        style: Style,
        overrides: Overrides,
        t_pose: bool,
        refine: bool,
        license: License,
    ) -> Self {
        let d = preset.defaults();
        Self {
            name,
            image,
            prompt,
            preset,
            style,
            polycount: overrides.polycount.unwrap_or(d.polycount),
            topology: overrides.topology.unwrap_or(d.topology),
            texture: overrides.texture.unwrap_or(d.texture),
            t_pose,
            refine,
            license,
        }
    }

    /// Sloyd `options` string, built the same way as the web app:
    /// `[-refine][-tpose]<preset flag>-license-<license>`.
    pub fn options(&self) -> String {
        let license = match self.license {
            License::Private => "-license-private",
            License::CcBy4 => "-license-cc-by-4.0",
        };
        [
            if self.refine { "-refine" } else { "" },
            if self.t_pose { "-tpose" } else { "" },
            self.preset.defaults().flag,
            license,
        ]
        .concat()
    }

    fn wire<T: Serialize>(v: T) -> String {
        serde_json::to_value(v)
            .ok()
            .and_then(|v| v.as_str().map(ToOwned::to_owned))
            .unwrap_or_default()
    }

    /// JSON body for `POST /jobs/text-to-3d`.
    pub fn text_body(&self) -> serde_json::Value {
        serde_json::json!({
            "prompt": self.prompt,
            "genStyleId": Self::wire(self.style),
            "options": self.options(),
            "targetFaceCount": self.polycount,
            "textureResolution": Self::wire(self.texture),
            "topology": Self::wire(self.topology),
            "tPose": self.t_pose,
        })
    }

    /// Multipart text fields for `POST /jobs/image-to-3d` (the image goes in `file`).
    pub fn image_fields(&self) -> Vec<(&'static str, String)> {
        vec![
            ("options", self.options()),
            ("targetFaceCount", self.polycount.to_string()),
            ("textureResolution", Self::wire(self.texture)),
            ("topology", Self::wire(self.topology)),
        ]
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Running,
    Completed,
    Failed { message: String },
}

impl JobStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed { .. })
    }
}

#[derive(Debug, Clone)]
pub struct Job {
    pub id: Uuid,
    pub status: JobStatus,
    pub request: GenerateRequest,
    pub elapsed_secs: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(preset: Preset) -> GenerateRequest {
        GenerateRequest::resolve(
            None,
            None,
            "chair".into(),
            preset,
            Style::Auto,
            Overrides::default(),
            false,
            false,
            License::Private,
        )
    }

    #[test]
    fn lowpoly_preset_matches_web_app() {
        let r = req(Preset::GameLowpoly);
        assert_eq!(r.options(), "-lowpoly-license-private");
        assert_eq!(
            r.text_body(),
            serde_json::json!({
                "prompt": "chair",
                "genStyleId": "Auto",
                "options": "-lowpoly-license-private",
                "targetFaceCount": 10000,
                "textureResolution": "1k",
                "topology": "triangles",
                "tPose": false,
            })
        );
    }

    #[test]
    fn flags_concatenate_in_web_app_order() {
        let mut r = req(Preset::PrintStandard);
        r.refine = true;
        r.t_pose = true;
        r.license = License::CcBy4;
        assert_eq!(r.options(), "-refine-tpose-3dprint-license-cc-by-4.0");
        assert_eq!(r.image_fields()[2].1, "none");
    }

    #[test]
    fn overrides_win_over_preset() {
        let r = GenerateRequest::resolve(
            None,
            None,
            "x".into(),
            Preset::GameLowpoly,
            Style::Polygonal,
            Overrides {
                polycount: Some(3_000),
                topology: Some(Topology::Quads),
                texture: Some(Texture::R512),
            },
            false,
            false,
            License::Private,
        );
        let b = r.text_body();
        assert_eq!(b["targetFaceCount"], 3000);
        assert_eq!(b["topology"], "quads");
        assert_eq!(b["textureResolution"], "512");
        assert_eq!(b["genStyleId"], "Polygonal");
    }
}
