struct Placement {
    top_left: vec2<f32>,
    bottom_right: vec2<f32>,
}

struct Varyings {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@group(0) @binding(0) var layer_texture: texture_2d<f32>;
@group(0) @binding(1) var layer_sampler: sampler;
@group(0) @binding(2) var<uniform> placement: Placement;

@vertex
fn vertex_main(@builtin(vertex_index) corner: u32) -> Varyings {
    let uv = vec2<f32>(f32(corner & 1u), f32(corner >> 1u));
    var varyings: Varyings;
    varyings.position = vec4<f32>(mix(placement.top_left, placement.bottom_right, uv), 0.0, 1.0);
    varyings.uv = uv;
    return varyings;
}

@fragment
fn fragment_main(varyings: Varyings) -> @location(0) vec4<f32> {
    return textureSample(layer_texture, layer_sampler, varyings.uv);
}
