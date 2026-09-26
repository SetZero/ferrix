#version 310 es
// `blur2.glsl`: the eight-tap upsample. The target's pixel at `v_pos` reads
// the source, half the size, around half its own place. u[0] is (1/source
// width, 1/source height, radius / 4, 0): a quarter, because the shader's
// offset is half a *target* pixel and the source's are twice as big.
precision highp float;
layout(location = 0) in vec4 v;
#define v_texcoord v.xy
#define v_pos v.zw
uniform sampler2D tex;
uniform vec4 u[1];
layout(location = 0) out vec4 color;
void main() {
    vec2 at = v_pos * 0.5;
    float r = u[0].z;
    vec4 sum = texture(tex, (at + vec2(-2.0 * r, 0.0)) * u[0].xy);
    sum += texture(tex, (at + vec2(-r, r)) * u[0].xy) * 2.0;
    sum += texture(tex, (at + vec2(0.0, 2.0 * r)) * u[0].xy);
    sum += texture(tex, (at + vec2(r, r)) * u[0].xy) * 2.0;
    sum += texture(tex, (at + vec2(2.0 * r, 0.0)) * u[0].xy);
    sum += texture(tex, (at + vec2(r, -r)) * u[0].xy) * 2.0;
    sum += texture(tex, (at + vec2(0.0, -2.0 * r)) * u[0].xy);
    sum += texture(tex, (at + vec2(-r, -r)) * u[0].xy) * 2.0;
    color = sum / 12.0;
}
