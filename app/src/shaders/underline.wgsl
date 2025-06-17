fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
  return pow(c, vec3<f32>(2.2));
}

struct Globals {
  screen_size: vec2<f32>,
  cell_size: vec2<f32>,
}
@group(0) @binding(0) var<uniform> globals: Globals;

struct UNderlineInstance {
  @location(5) position: vec2<f32>,
  @location(6) color: vec4<f32>,
};

struct VertexInput {
  @location(0) position: vec2<f32>,
};

struct VertexOutput {
  @builtin(position) clip_position: vec4<f32>,
  @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(
  model: VertexInput,
  instance: UNderlineInstance,
) -> VertexOutput {
  var out: VertexOutput;

  let line_height = 1.0;

  let line_box_pos = instance.position + vec2<f32>(0.0, globals.cell_size.y - line_height - 1.0);
  let line_box_size = vec2<f32>(globals.cell_size.x, line_height);

  let final_pos = model.position * line_box_size + line_box_pos;

  let clip_pos = vec2<f32>(
    (final_pos.x / globals.screen_size.x) * 2.0 - 1.0,
    (final_pos.y / globals.screen_size.y) * -2.0 + 1.0
  );

  out.clip_position = vec4<f32>(clip_pos, 0.0, 1.0);
  out.color = instance.color;

  return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
  let linear_rgb = srgb_to_linear(in.color.rgb);
  return vec4<f32>(linear_rgb, in.color.a);
}
