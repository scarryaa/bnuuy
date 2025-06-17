fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
  return pow(c, vec3<f32>(2.2));
}

struct Globals {
  screen_size: vec2<f32>,
  cell_size: vec2<f32>,
};
@group(0) @binding(0) var<uniform> globals: Globals;

struct UndercurlInstance {
  @location(3) position: vec2<f32>,
  @location(4) color: vec4<f32>,
};

struct VertexInput {
  @location(0) position: vec2<f32>,
};

struct VertexOutput {
  @builtin(position) clip_position: vec4<f32>,
  @location(0) uv: vec2<f32>,
  @location(1) color: vec4<f32>,
};

@vertex
fn vs_main(
  model: VertexInput,
  instance: UndercurlInstance,
) -> VertexOutput {
  var out: VertexOutput;

  let cell_box_size = vec2<f32>(globals.cell_size.x, 4.0);
  let cell_box_pos = instance.position + vec2<f32>(0.0, globals.cell_size.y - cell_box_size.y);

  let final_pos = model.position * cell_box_size + cell_box_pos;

  let clip_pos = vec2<f32>(
    (final_pos.x / globals.screen_size.x) * 2.0 - 1.0,
    (final_pos.y / globals.screen_size.y) * -2.0 + 1.0
  );

  out.clip_position = vec4<f32>(clip_pos, 0.0, 1.0);
  out.uv = model.position;
  out.color = instance.color;

  return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
  let frequency = 2.0 * 3.14159 * 2.5;

  let sine = sin(in.uv.x * frequency);
  let squiggle_y = (sine * 0.25) + 0.5;
  let distance_from_squiggle = abs(in.uv.y - squiggle_y);

  let alpha = 1.0 - smoothstep(0.0, 0.2, distance_from_squiggle);

  if (alpha < 0.1) {
    discard;
  }

  let linear_rgb = srgb_to_linear(in.color.rgb);
  return vec4<f32>(linear_rgb, alpha);
}
