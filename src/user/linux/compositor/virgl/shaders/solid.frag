#version 310 es
// One premultiplied colour, u[0], inside a rounded rectangle: u[1] is the
// rectangle and u[2] (radius, power). A radius of zero is a plain fill.
precision highp float;
layout(location = 0) in vec4 v;
#define v_texcoord v.xy
#define v_pos v.zw
uniform vec4 u[3];
layout(location = 0) out vec4 color;
#include "shape.glsl"
void main() {
    color = u[0] * inside(v_pos, u[1], u[2].x, u[2].y);
}
