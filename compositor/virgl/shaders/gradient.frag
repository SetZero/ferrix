#version 310 es
// A border's gradient: `tex` is the ramp compositor/render builds, one row
// of premultiplied colours, and the place along it is `gradient.glsl`'s
// folded progress across the box u[0]: u[1] is (sine, flip x, flip y, 0).
// Drawn inside the rounded rectangle u[2] with (radius, power) in u[3].xy.
//
// The ramp is read at its nearest entry, as compositor/render reads it:
// entry round(progress * last) of `length`, whose centre is at
// (progress * last + 0.5) / length. u[3].zw is (last / length, 0.5 / length).
precision highp float;
layout(location = 0) in vec4 v;
#define v_texcoord v.xy
#define v_pos v.zw
uniform sampler2D tex;
uniform vec4 u[4];
layout(location = 0) out vec4 color;
#include "shape.glsl"
void main() {
    vec2 at = (v_pos - u[0].xy) / u[0].zw;
    at = mix(at, 1.0 - at, u[1].yz);
    float progress = clamp(at.y * u[1].x + at.x * (1.0 - u[1].x), 0.0, 1.0);
    float along = progress * u[3].z + u[3].w;
    color = texture(tex, vec2(along, 0.5)) * inside(v_pos, u[2], u[3].x, u[3].y);
}
