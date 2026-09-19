#version 310 es
// A client's surface: its texture, times an opacity in u[0].x, inside a
// rounded rectangle u[1] with (radius, power) in u[2].
precision highp float;
layout(location = 0) in vec4 v;
#define v_texcoord v.xy
#define v_pos v.zw
uniform sampler2D tex;
uniform vec4 u[3];
layout(location = 0) out vec4 color;
#include "shape.glsl"
void main() {
    color = texture(tex, v_texcoord) * (u[0].x * inside(v_pos, u[1], u[2].x, u[2].y));
}
