use linked_hash_map::LinkedHashMap;
use serde_derive::*;

/// semver
pub const CONFIG_VERSION: &str = "v0.0.1";
/// (major, minor, patch)
pub const CONFIG_VERSION_TRIPLE: (u32, u32, u32) = (0, 0, 1);

pub type Mat3 = [[f64; 3]; 3];
pub type Mat4 = [[f64; 4]; 4];

#[derive(Deserialize, Debug)]
pub struct ShaderPackConfig {
    pub version: String,
    pub support: String,
    pub resources: ResourcesConfig,
    pub pipelines: PipelinesConfig,
}

impl ShaderPackConfig {
    /// Returns true if the first two numbers (major and minor) are as expected.
    /// If the format is incorrect or they're different, this returns false.
    pub fn is_correct_version(&self) -> bool {
        let numbers: Vec<u32> = self
            .version
            .strip_prefix('v')
            .unwrap_or_default() // if it couldn't find the default, numbers will be empty
            .split('.')
            .map(|number| &number[..number.len() - 1])
            .map(|num_str| num_str.parse().unwrap_or(u32::MAX))
            .collect();

        numbers.len() == 3
            && numbers[0] == CONFIG_VERSION_TRIPLE.0
            && numbers[1] == CONFIG_VERSION_TRIPLE.1
            && numbers[2] != u32::MAX
    }
}

#[derive(Deserialize, Debug)]
pub struct ResourcesConfig {
    #[serde(flatten)]
    pub resources: LinkedHashMap<String, ShorthandResourceConfig>,
}

#[derive(Deserialize, Debug)]
#[serde(untagged)]
pub enum ShorthandResourceConfig {
    Int(i64),
    Float(f64),
    Mat3(Mat3),
    Mat4(Mat4),
    Longhand(LonghandResourceConfig),
}

#[derive(Deserialize, Debug)]
pub struct LonghandResourceConfig {
    #[serde(flatten)]
    pub common: CommonResourceConfig,

    #[serde(flatten)]
    pub typed: TypeResourceConfig,
}

#[derive(Deserialize, Debug)]
pub struct CommonResourceConfig {
    #[serde(default)]
    pub desc: String,

    #[serde(default)]
    pub show: bool,
}

#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TypeResourceConfig {
    #[serde(rename = "texture_3d")]
    Texture3d {
        #[serde(default)]
        src: String,
        #[serde(default)]
        clear_after_frame: bool,
    },
    #[serde(rename = "texture_2d")]
    Texture2d {
        #[serde(default)]
        src: String,
        #[serde(default)]
        clear_after_frame: bool,
    },
    TextureDepth {
        #[serde(default)]
        src: String,
        #[serde(default)]
        clear_after_frame: bool,
    },
    Float {
        range: [f64; 2],
        value: f64,
    },
    Int {
        range: [i64; 2],
        value: i64,
    },
    Mat3(Mat3ValueOrMult),
    Mat4(Mat4ValueOrMult),
}

#[derive(Deserialize, Debug)]
#[serde(untagged)]
pub enum Mat3ValueOrMult {
    Value { value: Mat3 },
    Mult { mult: Vec<String> },
}

#[derive(Deserialize, Debug)]
#[serde(untagged)]
pub enum Mat4ValueOrMult {
    Value { value: Mat4 },
    Mult { mult: Vec<String> },
}

#[derive(Deserialize, Debug)]
pub struct PipelinesConfig {
    #[serde(flatten)]
    pub pipelines: LinkedHashMap<String, PipelineConfig>,
}

#[derive(Deserialize, Debug)]
pub struct PipelineConfig {
    pub geometry: String,

    #[serde(default)]
    pub output: Vec<String>,

    #[serde(default)]
    pub depth: String,

    #[serde(default)]
    pub uniforms: LinkedHashMap<u64, Uniform>,
}

#[derive(Deserialize, Debug)]
pub struct Uniform {
    pub resource: String,
    pub visibility: Vec<UniformVisibility>,
}

#[derive(Deserialize, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum UniformVisibility {
    Vert,
    Frag,
}

#[cfg(test)]
mod tests {
    use super::ShaderPackConfig;
    use serde::Deserialize;
    use std::fmt::Debug;

    fn deserialize_and_print_error<'a, T: Debug + Deserialize<'a>>(input: &'a str) {
        let config: Result<T, _> = serde_yaml::from_str(input);
        println!("{:?}", config);
        if let Err(err) = config {
            if let Some(loc) = err.location() {
                let lines: Vec<&str> = input.lines().collect();
                let line = lines[loc.line()];
                println!("{}:{}{:?}", loc.line(), loc.column(), line);
            }
            panic!();
        }
    }

    const FULL_YAML: &str = r#"
version: "0.0.1"
support: glsl # could also be wgsl
resources:
  shadowmap_texture_depth:
    type: texture_depth
    clear_after_frame: true
  shadow_ortho_mat4:
    type: mat4
    value: # this is just an identity matrix, it would be something different in practice
      - [1.0, 0.0, 0.0, 0.0]
      - [0.0, 1.0, 0.0, 0.0]
      - [0.0, 0.0, 1.0, 0.0]
      - [0.0, 0.0, 0.0, 1.0]
  model_view_mat4:
    type: mat4
    mult: [wm_model_mat4, wm_view_mat4]
  mvp_mat4:
    type: mat4
    mult: [wm_model_mat4, wm_view_mat4, wm_projection_mat4]
pipelines:
  terrain_shadows:
    geometry: wm_geo_terrain # one
    depth: shadowmap_texture_depth
    uniforms:
      0:
        resource: model_view_mat4
        visibility: [vert]
      1:
        resource: shadow_ortho_mat4
        visibility: [vert]

  entity_shadows:
    geometry: wm_geo_entities
    depth: shadowmap_texture_depth
    uniforms:
      0:
        resource: model_view_mat4
        visibility: [vert]
      1:
        resource: shadow_ortho_mat4
        visibility: [vert]
      2:
        resource: wm_ssbo_entity_part_transforms
        visibility: [vert]

  terrain:
    geometry: wm_geo_terrain
    depth: wm_framebuffer_depth
    output: [wm_framebuffer_texture]
  entities:
    geometry: wm_geo_entities
    depth: wm_framebuffer_depth
    output: [wm_framebuffer_texture]
    uniforms:
      0:
        resource: model_view_mat4
        visibility: [vert, frag]
      1:
        resource: shadow_ortho_mat4
        visibility: [vert, frag]
      2:
        resource: wm_ssbo_entity_part_transforms
        visibility: [vert, frag]
      3:
        resource: mvp_mat4
        visibility: [vert, frag]
      4:
        resource: shadowmap_texture_depth
        visibility: [frag]
"#;

    #[test]
    fn complete_file() {
        deserialize_and_print_error::<ShaderPackConfig>(FULL_YAML);
    }
}
