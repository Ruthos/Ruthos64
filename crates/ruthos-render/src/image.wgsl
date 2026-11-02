// Window/level + CLUT shader for one 2D frame.
//
// The quad covers the image in pixel space; `m` maps pixel coordinates to
// clip space inside the viewport the callback is drawn into.

struct Uniforms {
    m: mat4x4<f32>,
    // x: window centre, y: window width, z: inverted (MONOCHROME1), w: is colour
    wl: vec4<f32>,
    // x: width, y: height, z: bilinear (1) or nearest (0), w: unused
    size: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var img: texture_2d<f32>;
@group(0) @binding(2) var clut: texture_2d<f32>;
@group(0) @binding(3) var clut_sampler: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 1.0), vec2<f32>(0.0, 1.0),
    );
    let c = corners[vi];
    let img_pos = c * u.size.xy;
    var out: VsOut;
    out.pos = u.m * vec4<f32>(img_pos, 0.0, 1.0);
    out.uv = c;
    return out;
}

fn load(p: vec2<i32>) -> vec4<f32> {
    let maxp = vec2<i32>(u.size.xy) - vec2<i32>(1, 1);
    let cp = clamp(p, vec2<i32>(0, 0), maxp);
    return textureLoad(img, cp, 0);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let p = in.uv * u.size.xy - vec2<f32>(0.5, 0.5);
    var s: vec4<f32>;
    if (u.size.z > 0.5) {
        let p0 = floor(p);
        let f = p - p0;
        let i0 = vec2<i32>(p0);
        let a = load(i0);
        let b = load(i0 + vec2<i32>(1, 0));
        let c = load(i0 + vec2<i32>(0, 1));
        let d = load(i0 + vec2<i32>(1, 1));
        s = mix(mix(a, b, f.x), mix(c, d, f.x), f.y);
    } else {
        s = load(vec2<i32>(round(p)));
    }

    let center = u.wl.x;
    let width = max(u.wl.y, 1.0);

    if (u.wl.w > 0.5) {
        // Colour: the window applies to each channel over 0..255, as Horos does.
        let v = s.rgb * 255.0;
        let t = clamp((v - vec3<f32>(center - 0.5)) / (width - 1.0) + vec3<f32>(0.5), vec3<f32>(0.0), vec3<f32>(1.0));
        return vec4<f32>(t, 1.0);
    }
    var t = (s.r - (center - 0.5)) / (width - 1.0) + 0.5;
    t = clamp(t, 0.0, 1.0);
    if (u.wl.z > 0.5) {
        t = 1.0 - t;
    }
    // 256-entry table: sample at texel centres.
    let tx = (t * 255.0 + 0.5) / 256.0;
    let col = textureSampleLevel(clut, clut_sampler, vec2<f32>(tx, 0.5), 0.0);
    return vec4<f32>(col.rgb, 1.0);
}
