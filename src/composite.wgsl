struct Params {
    camera: vec4f,
    window_rect: vec4f,
    cursor: vec4f,
    sizes: vec4f,
    clicks: array<vec4f, 4>,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var window_texture: texture_2d<f32>;
@group(0) @binding(2) var linear_sampler: sampler;
@group(0) @binding(3) var background_texture: texture_2d<f32>;
@group(0) @binding(4) var<storage, read_write> output: array<u32>;

const CORNER_RADIUS: f32 = 14.0;
const SHADOW_OFFSET: f32 = 18.0;
const SHADOW_BLUR: f32 = 48.0;
const SHADOW_ALPHA: f32 = 0.45;
const ARROW: array<vec2f, 7> = array<vec2f, 7>(
    vec2f(0.0, 0.0),
    vec2f(0.0, 16.0),
    vec2f(4.0, 12.5),
    vec2f(7.0, 19.0),
    vec2f(9.5, 18.0),
    vec2f(6.5, 11.5),
    vec2f(11.5, 11.5),
);
const ARROW_HEIGHT: f32 = 19.0;
const CURSOR_HEIGHT: f32 = 30.0;
const CURSOR_OUTLINE: f32 = 1.2;
const RIPPLE_START: f32 = 6.0;
const RIPPLE_GROWTH: f32 = 30.0;
const RIPPLE_RING: f32 = 2.2;

fn window_distance(scene: vec2f) -> f32 {
    let half_size = params.window_rect.zw * 0.5;
    let q = abs(scene - params.window_rect.xy - half_size) - half_size + CORNER_RADIUS;
    return length(max(q, vec2f(0.0))) + min(max(q.x, q.y), 0.0) - CORNER_RADIUS;
}

fn arrow_distance(point: vec2f) -> f32 {
    var arrow = ARROW;
    var dist_sq = dot(point - arrow[0], point - arrow[0]);
    var sign = 1.0;
    var j = 6u;
    for (var i = 0u; i < 7u; i++) {
        let a = arrow[i];
        let edge = arrow[j] - a;
        let w = point - a;
        let t = clamp(dot(w, edge) / dot(edge, edge), 0.0, 1.0);
        let b = w - edge * t;
        dist_sq = min(dist_sq, dot(b, b));
        let c = vec3<bool>((point.y >= a.y), (point.y < arrow[j].y), (edge.x * w.y > edge.y * w.x));
        if all(c) || !any(c) {
            sign = -sign;
        }
        j = i;
    }
    return sign * sqrt(dist_sq);
}

fn color_at(pixel: vec2f) -> vec3f {
    let px_per_unit = params.camera.z * params.sizes.x / params.sizes.z;
    let half_output = params.sizes.xy * 0.5;
    let scene = params.camera.xy + (pixel - half_output) / px_per_unit;

    let shadow_distance = window_distance(scene - vec2f(0.0, SHADOW_OFFSET));
    let shadow = SHADOW_ALPHA * (1.0 - smoothstep(0.0, SHADOW_BLUR, shadow_distance));
    let background = textureSampleLevel(background_texture, linear_sampler, scene / params.sizes.zw, 0.0).rgb;
    var color = background * (1.0 - shadow);

    let coverage = clamp(0.5 - window_distance(scene) * px_per_unit, 0.0, 1.0);
    if coverage > 0.0 {
        let uv = (scene - params.window_rect.xy) / params.window_rect.zw;
        let window = textureSampleLevel(window_texture, linear_sampler, uv, 0.0).rgb;
        color = mix(color, window, coverage);
    }

    for (var i = 0u; i < 4u; i++) {
        let click = params.clicks[i];
        if click.w > 0.0 {
            let center = (click.xy - params.camera.xy) * px_per_unit + half_output;
            let grow = 1.0 - pow(1.0 - click.z, 3.0);
            let radius = (RIPPLE_START + RIPPLE_GROWTH * grow) * px_per_unit;
            let fade = pow(1.0 - click.z, 2.0);
            let distance = length(pixel - center);
            let ring = 1.0 - smoothstep(0.0, RIPPLE_RING * px_per_unit, abs(distance - radius));
            let fill = 1.0 - smoothstep(radius - px_per_unit, radius + px_per_unit, distance);
            color = mix(color, vec3f(1.0), clamp(fade * (0.85 * ring + 0.22 * fill), 0.0, 1.0));
        }
    }

    if params.cursor.z > 0.0 {
        let scale = CURSOR_HEIGHT / ARROW_HEIGHT * px_per_unit;
        let tip = (params.cursor.xy - params.camera.xy) * px_per_unit + half_output;
        let local = (pixel - tip) / scale;
        if all(local > vec2f(-2.0)) && all(local < vec2f(14.0, ARROW_HEIGHT + 2.0)) {
            let d = arrow_distance(local) * scale;
            let alpha = clamp(0.5 - d, 0.0, 1.0);
            let fill = clamp(0.5 - (d + CURSOR_OUTLINE * scale), 0.0, 1.0);
            color = mix(color, vec3f(fill), alpha);
        }
    }
    return color;
}

fn luma(rgb: vec3f) -> f32 {
    return dot(rgb, vec3f(0.2126, 0.7152, 0.0722));
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3u) {
    let words_per_row = u32(params.sizes.x) / 4u;
    let height = u32(params.sizes.y);
    if id.x >= words_per_row || id.y >= height + height / 2u {
        return;
    }

    var bytes: vec4f;
    if id.y < height {
        for (var i = 0u; i < 4u; i++) {
            let pixel = vec2f(f32(id.x * 4u + i), f32(id.y)) + 0.5;
            bytes[i] = 16.0 + 219.0 * luma(color_at(pixel));
        }
    } else {
        let row = f32(2u * (id.y - height));
        for (var i = 0u; i < 2u; i++) {
            let column = f32(4u * id.x + 2u * i);
            let rgb = (color_at(vec2f(column + 0.5, row + 0.5)) + color_at(vec2f(column + 1.5, row + 0.5))
                + color_at(vec2f(column + 0.5, row + 1.5)) + color_at(vec2f(column + 1.5, row + 1.5))) * 0.25;
            let y = luma(rgb);
            bytes[2u * i] = 128.0 + 224.0 * (rgb.b - y) / 1.8556;
            bytes[2u * i + 1u] = 128.0 + 224.0 * (rgb.r - y) / 1.5748;
        }
    }
    output[id.y * words_per_row + id.x] = pack4x8unorm(bytes / 255.0);
}

@compute @workgroup_size(16, 16)
fn main_rgba(@builtin(global_invocation_id) id: vec3u) {
    let width = u32(params.sizes.x);
    if id.x >= width || id.y >= u32(params.sizes.y) {
        return;
    }
    output[id.y * width + id.x] = pack4x8unorm(vec4f(color_at(vec2f(id.xy) + 0.5), 1.0));
}
