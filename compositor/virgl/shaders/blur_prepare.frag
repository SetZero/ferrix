#version 310 es
// `blurprepare.glsl`: contrast through the gain curve, then brightness
// where it brightens. Reads the pixel it writes: u[0] is (1/width,
// 1/height, contrast, max(1, brightness)).
precision highp float;
layout(location = 0) in vec4 v;
#define v_texcoord v.xy
#define v_pos v.zw
uniform sampler2D tex;
uniform vec4 u[1];
layout(location = 0) out vec4 color;
void main() {
    vec4 pixel = texture(tex, v_pos * u[0].xy);
    if (u[0].z != 1.0) {
        vec3 x = clamp(pixel.rgb, 0.0, 1.0);
        vec3 t = step(0.5, x);
        vec3 y = mix(x, 1.0 - x, t);
        vec3 a = 0.5 * pow(2.0 * y, vec3(u[0].z));
        pixel.rgb = mix(a, 1.0 - a, t);
    }
    pixel.rgb *= u[0].w;
    color = pixel;
}
