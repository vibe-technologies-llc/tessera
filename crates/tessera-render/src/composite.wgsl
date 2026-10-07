struct Placement {
    top_left: vec2<f32>,
    top_right: vec2<f32>,
    bottom_left: vec2<f32>,
    bottom_right: vec2<f32>,
    uv_rect: vec4<f32>,
    opacity: f32,
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
    var corners = array<vec2<f32>, 4>(
        placement.top_left,
        placement.top_right,
        placement.bottom_left,
        placement.bottom_right,
    );
    let unit = vec2<f32>(f32(corner & 1u), f32(corner >> 1u));
    var varyings: Varyings;
    varyings.position = vec4<f32>(corners[corner], 0.0, 1.0);
    varyings.uv = mix(placement.uv_rect.xy, placement.uv_rect.zw, unit);
    return varyings;
}

@fragment
fn fragment_main(varyings: Varyings) -> @location(0) vec4<f32> {
    let color = textureSample(layer_texture, layer_sampler, varyings.uv);
    return vec4<f32>(color.rgb, color.a * placement.opacity);
}
