// Volume rendering: rays through the intensity texture, front to back, with
// a transfer function, gradient lighting and early termination. The label
// texture shows, hides and colours segments.

struct Uniforms {
    // Maps (ndc x, ndc y, depth in mm, 1) to texture coordinates.
    m: mat4x4<f32>,
    // x: step in mm, y: mode (0 shaded, 1 maximum, 2 minimum),
    // z: use segments (0 or 1), w: reference step for opacity correction
    params: vec4<f32>,
    // Voxels along each axis.
    dims: vec4<f32>,
    // Millimetres per voxel along each axis.
    spacing: vec4<f32>,
    // A box in texture coordinates. clip_low.w: 0 none, 1 keep what is inside
    // it, 2 remove what is inside it.
    clip_low: vec4<f32>,
    clip_high: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var volume: texture_3d<f32>;
@group(0) @binding(2) var labels: texture_3d<u32>;
@group(0) @binding(3) var transfer: texture_2d<f32>;
@group(0) @binding(4) var segments: texture_2d<f32>;
@group(0) @binding(5) var linear: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) ndc: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    var corners = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    var out: VsOut;
    out.pos = vec4<f32>(corners[vi], 0.0, 1.0);
    out.ndc = corners[vi];
    return out;
}

fn value_at(p: vec3<f32>) -> f32 {
    return textureSampleLevel(volume, linear, p, 0.0).r;
}

struct Tint {
    colour: vec3<f32>,
    // How much of the point is coloured, from 0 to 1.
    strength: f32,
    // How much of it is shown.
    alpha: f32,
};

// The segments at a point: the eight voxels around it, blended, so a segment's
// edge is as smooth as the surface it lies on. Black in the table means no
// colour of its own.
fn segment_at(p: vec3<f32>) -> Tint {
    let at = p * u.dims.xyz - vec3<f32>(0.5);
    let base = floor(at);
    let f = at - base;
    let top = vec3<i32>(u.dims.xyz) - vec3<i32>(1);
    var out = Tint(vec3<f32>(0.0), 0.0, 0.0);
    for (var i = 0; i < 8; i++) {
        let corner = vec3<i32>(i & 1, (i >> 1) & 1, (i >> 2) & 1);
        let c = vec3<f32>(corner);
        let w = mix(1.0 - f.x, f.x, c.x) * mix(1.0 - f.y, f.y, c.y) * mix(1.0 - f.z, f.z, c.z);
        let voxel = clamp(vec3<i32>(base) + corner, vec3<i32>(0), top);
        let label = textureLoad(labels, voxel, 0).r;
        let entry = textureSampleLevel(segments, linear, vec2<f32>((f32(label) + 0.5) / 256.0, 0.5), 0.0);
        out.alpha += w * entry.a;
        if (entry.a > 0.0 && dot(entry.rgb, entry.rgb) > 0.0) {
            out.colour += w * entry.rgb;
            out.strength += w;
        }
    }
    if (out.strength > 0.0) {
        out.colour /= out.strength;
    }
    return out;
}

// The gradient in millimetres along the volume's axes.
fn gradient(p: vec3<f32>) -> vec3<f32> {
    let d = 1.0 / u.dims.xyz;
    return vec3<f32>(
        (value_at(p + vec3<f32>(d.x, 0.0, 0.0)) - value_at(p - vec3<f32>(d.x, 0.0, 0.0))) / u.spacing.x,
        (value_at(p + vec3<f32>(0.0, d.y, 0.0)) - value_at(p - vec3<f32>(0.0, d.y, 0.0))) / u.spacing.y,
        (value_at(p + vec3<f32>(0.0, 0.0, d.z)) - value_at(p - vec3<f32>(0.0, 0.0, d.z))) / u.spacing.z,
    );
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let start = (u.m * vec4<f32>(in.ndc, 0.0, 1.0)).xyz;
    let dir = (u.m * vec4<f32>(0.0, 0.0, 1.0, 0.0)).xyz;

    // Where the ray enters and leaves what can be seen, in millimetres of depth:
    // the unit cube, or a box kept inside it.
    let keep = u.clip_low.w > 0.5 && u.clip_low.w < 1.5;
    let remove = u.clip_low.w > 1.5;
    let box_low = select(vec3<f32>(0.0), u.clip_low.xyz, keep);
    let box_high = select(vec3<f32>(1.0), u.clip_high.xyz, keep);
    let inv = 1.0 / dir;
    let t0 = (box_low - start) * inv;
    let t1 = (box_high - start) * inv;
    let near = min(t0, t1);
    let far = max(t0, t1);
    let enter = max(max(near.x, near.y), near.z);
    let leave = min(min(far.x, far.y), far.z);
    if (enter >= leave) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }

    let step = u.params.x;
    let mode = u.params.y;
    let use_segments = u.params.z > 0.5;
    let correction = step / u.params.w;
    let count = min(u32((leave - enter) / step) + 1u, 4096u);

    // The view direction along the volume's axes, in millimetres.
    let view = normalize(dir * u.dims.xyz * u.spacing.xyz);

    var colour = vec3<f32>(0.0);
    var alpha = 0.0;
    var best = select(0.0, 1.0, mode > 1.5);
    // Each ray starts a different fraction of a step in. Rays in step with each
    // other meet a surface in terraces, which show as contour lines.
    let jitter = fract(52.9829189 * fract(dot(in.pos.xy, vec2<f32>(0.06711056, 0.00583715))));
    var i = 0u;
    loop {
        if (i >= count) {
            break;
        }
        let p = start + dir * (enter + (f32(i) + jitter) * step);
        i += 1u;
        if (remove && all(p >= u.clip_low.xyz) && all(p <= u.clip_high.xyz)) {
            continue;
        }
        // Most of a ray is air. Looking up segments costs far more than this,
        // so it waits until there is something to show.
        let v = value_at(p);
        var c = vec4<f32>(0.0);
        if (mode < 0.5) {
            c = textureSampleLevel(transfer, linear, vec2<f32>(v, 0.5), 0.0);
            if (c.a <= 0.002) {
                continue;
            }
        }
        var tint = Tint(vec3<f32>(0.0), 0.0, 1.0);
        if (use_segments) {
            tint = segment_at(p);
            if (tint.alpha <= 0.0) {
                continue;
            }
        }
        if (mode > 0.5) {
            if (mode < 1.5) {
                best = max(best, v);
            } else {
                best = min(best, v);
            }
            continue;
        }
        if (use_segments) {
            // A segment's colour over the intensities, and its opacity.
            c = vec4<f32>(mix(c.rgb, tint.colour, 0.65 * tint.strength), c.a * tint.alpha);
        }
        let g = gradient(p);
        let size = length(g);
        var light = 1.0;
        if (size > 0.00001) {
            let n = -g / size;
            let diffuse = abs(dot(n, view));
            let specular = pow(diffuse, 24.0) * 0.3;
            light = 0.25 + 0.75 * diffuse + specular;
        }
        let a = 1.0 - pow(1.0 - c.a, correction);
        colour += (1.0 - alpha) * a * c.rgb * light;
        alpha += (1.0 - alpha) * a;
        if (alpha > 0.97) {
            break;
        }
    }
    if (mode > 0.5) {
        let c = textureSampleLevel(transfer, linear, vec2<f32>(best, 0.5), 0.0);
        return vec4<f32>(c.rgb, 1.0);
    }
    return vec4<f32>(colour, 1.0);
}
