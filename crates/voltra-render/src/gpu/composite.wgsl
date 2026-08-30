// Drawing one scene item as a textured quad.
//
// The vertex stage receives the four canvas-space corners the compositor
// already computed from `Placement::forward`, so no matrix work happens here:
// the CPU resolved the transform once per item and the GPU just uses it.
//
// The fragment stage premultiplies by alpha. That is not decoration: the blend
// equations in `pipeline.rs` come from libobs's table, where the source colour
// factor is ONE, and ONE is only correct against a premultiplied source.

struct Item {
    // The four canvas-space corners, in the order top-left, top-right,
    // bottom-right, bottom-left. `w` and `z` carry the matching UV.
    corners: array<vec4<f32>, 4>,
    // Canvas size in pixels, used to map canvas space to clip space.
    canvas: vec2<f32>,
    // Extra opacity applied to the whole item, 0..=1.
    opacity: f32,
    _padding: f32,
};

@group(0) @binding(0) var<uniform> item: Item;
@group(0) @binding(1) var source: texture_2d<f32>;
@group(0) @binding(2) var source_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vertex_main(@builtin(vertex_index) index: u32) -> VertexOutput {
    // A triangle strip over the four corners: 0, 1, 3, 2 walks them so the two
    // triangles cover the quad without a second vertex buffer.
    var order = array<u32, 4>(0u, 1u, 3u, 2u);
    let corner = item.corners[order[index]];

    // Canvas pixels to clip space. Y flips because canvas space grows downward
    // and clip space grows upward.
    let x = corner.x / item.canvas.x * 2.0 - 1.0;
    let y = 1.0 - corner.y / item.canvas.y * 2.0;

    var out: VertexOutput;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>(corner.z, corner.w);
    return out;
}

@fragment
fn fragment_main(in: VertexOutput) -> @location(0) vec4<f32> {
    var colour = textureSample(source, source_sampler, in.uv);
    colour.a = colour.a * item.opacity;
    // Premultiply, so the ONE source factor of the blend table is right.
    return vec4<f32>(colour.rgb * colour.a, colour.a);
}
