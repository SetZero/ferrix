#version 310 es
// `blurFinish.glsl`, and the window's shape: a hash dither fixed to the
// screen, then brightness where it darkens, written -- not blended -- inside
// the rounded rectangle u[1] with (radius, power) in u[2], and nowhere
// else. u[0] is (1/width, 1/height, noise, min(1, brightness)).
precision highp float;
layout(location = 0) in vec4 v;
#define v_texcoord v.xy
#define v_pos v.zw
uniform sampler2D tex;
uniform vec4 u[3];
layout(location = 0) out vec4 color;
#include "shape.glsl"
float hash(vec2 p) {
    vec3 p3 = fract(vec3(p.xyx) * 1689.1984);
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}
void main() {
    if (inside(v_pos, u[1], u[2].x, u[2].y) == 0.0)
        discard;
    vec2 at = v_pos * u[0].xy;
    vec4 pixel = texture(tex, at);
    pixel.rgb += (hash(at) - 0.5) * u[0].z;
    pixel.rgb *= u[0].w;
    color = vec4(pixel.rgb, 1.0);
}
