struct Glass {
    panel: vec4f,
    params: vec4f,
}

@group(0) @binding(0) var<uniform> glass: Glass;

const TINT: vec3f = vec3f(0.07, 0.08, 0.11);
const TINT_ALPHA: f32 = 0.6;

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4f {
    let corner = vec2f(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4f(corner * 2.0 - 1.0, 0.0, 1.0);
}

fn rounded_box(p: vec2f, half_size: vec2f, radius: f32) -> f32 {
    let q = abs(p) - half_size + radius;
    return length(max(q, vec2f(0.0))) + min(max(q.x, q.y), 0.0) - radius;
}

@fragment
fn fs_main(@builtin(position) frag: vec4f) -> @location(0) vec4f {
    let radius = glass.params.x;
    let time = glass.params.y;
    let opacity = glass.params.z;
    let ppp = glass.params.w;
    let size = glass.panel.zw;
    let half_size = size * 0.5;
    let center = glass.panel.xy + half_size;
    let p = frag.xy;

    let d = rounded_box(p - center, half_size, radius);
    let coverage = clamp(0.5 - d, 0.0, 1.0);

    let uv = (p - glass.panel.xy) / size;
    let sheen = 0.15 * (1.0 - smoothstep(0.0, 0.55, uv.y));
    let glow_center = glass.panel.xy + size * vec2f(0.15, -0.2);
    let glow = 0.13 * (1.0 - smoothstep(0.0, 1.0, length((p - glow_center) / (size * vec2f(0.6, 1.1)))));

    let phase = 0.5 - 0.5 * cos(time * 6.2831853 / 7.0);
    let band_center = mix(1.25, -0.25, phase);
    let slant = (uv.y - 0.5) * 0.364 * size.y / size.x;
    let band = 0.085 * exp(-pow((uv.x - slant - band_center) / 0.075, 2.0));

    let edge_fade = mix(0.44, 0.1, smoothstep(0.0, 1.0, uv.y));
    let edge = (1.0 - smoothstep(0.0, 1.25 * ppp, -d)) * edge_fade;

    let highlight = clamp(sheen + glow + band + edge, 0.0, 0.7);
    let tint = TINT * TINT_ALPHA * (1.0 - highlight);
    let alpha = TINT_ALPHA + highlight * (1.0 - TINT_ALPHA);
    return vec4f(tint + vec3f(highlight), alpha) * coverage * opacity;
}
