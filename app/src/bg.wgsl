@group(0) @binding(0)
var<uniform> globals: vec4<f32>; // screen_w, screen_h, cell_w, cell_h

struct VertexInput {
  @location(0) position: vec2<f32>,
  @location(1) instance_pos: vec2<f32>,
  @location(2) instance_color: vec4<f32>,
};

struct VertexOutput {
  @builtin(position) clip_position: vec4<f32>,
  @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(model: VertexInput) -> VertexOutput {
  let screen_size = globals.xy;
  let cell_size = globals.zw;

  let pixel_pos = model.instance_pos + (model.position * cell_size);

  let zero_to_two = pixel_pos / screen_size * 2.0;
  let clip_space = zero_to_two - 1.0;

  var out: VertexOutput;
  // Flip y coord
  out.clip_position = vec4<f32>(clip_space.x, -clip_space.y, 0.0, 1.0);
  out.color = model.instance_color;
  return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
  return in.color;
}
