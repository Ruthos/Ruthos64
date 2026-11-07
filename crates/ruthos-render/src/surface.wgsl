// Segment surfaces in the 3D pane, lit from the viewer.

struct Uniforms {
    // Patient millimetres to clip space.
    m: mat4x4<f32>,
    // The way the view looks, in patient space.
    forward: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;

struct VsIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) colour: vec4<f32>,
};

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) colour: vec4<f32>,
};

@vertex
fn vs_main(v: VsIn) -> VsOut {
    var out: VsOut;
    out.pos = u.m * vec4<f32>(v.position, 1.0);
    out.normal = v.normal;
    out.colour = v.colour;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let n = normalize(in.normal);
    let towards = -u.forward.xyz;
    // Both sides lit, so an open surface still reads.
    let facing = abs(dot(n, towards));
    let diffuse = 0.25 + 0.75 * facing;
    let shine = pow(facing, 24.0) * 0.25;
    return vec4<f32>(in.colour.rgb * diffuse + vec3<f32>(shine), 1.0);
}
