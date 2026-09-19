#version 310 es
// `blur1.glsl`: the five-tap downsample, ending in the vibrancy boost.
// Written in pixels: the target's pixel at `v_pos` reads the source, twice
// the size, around twice its own place. u[0] is (1/source width, 1/source
// height, radius, 0) and u[1] (vibrancy / passes, 1 - vibrancy_darkness,
// 0, 0).
precision highp float;
layout(location = 0) in vec4 v;
#define v_texcoord v.xy
#define v_pos v.zw
uniform sampler2D tex;
uniform vec4 u[2];
layout(location = 0) out vec4 color;

const float Pr = 0.299;
const float Pg = 0.587;
const float Pb = 0.114;
const float A = 0.93;
const float B = 0.11;
const float C = 0.66;

float doubleCircleSigmoid(float x, float a) {
    a = clamp(a, 0.0, 1.0);
    if (x <= a)
        return a - sqrt(a * a - x * x);
    return a + sqrt(pow(1.0 - a, 2.0) - pow(x - 1.0, 2.0));
}

vec3 rgb2hsl(vec3 col) {
    float minc = min(col.r, min(col.g, col.b));
    float maxc = max(col.r, max(col.g, col.b));
    float delta = maxc - minc;
    float lum = (minc + maxc) * 0.5;
    float sat = 0.0;
    float hue = 0.0;
    if (lum > 0.0 && lum < 1.0) {
        float mul = (lum < 0.5) ? lum : (1.0 - lum);
        sat = delta / (mul * 2.0);
    }
    if (delta > 0.0) {
        vec3 maxcVec = vec3(maxc);
        vec3 masks = vec3(equal(maxcVec, col)) * vec3(notEqual(maxcVec, col.gbr));
        vec3 adds = vec3(0.0, 2.0, 4.0) + (col.gbr - col.brg) / delta;
        hue += dot(adds, masks);
        hue /= 6.0;
        if (hue < 0.0)
            hue += 1.0;
    }
    return vec3(hue, sat, lum);
}

vec3 hsl2rgb(vec3 col) {
    const float onethird = 1.0 / 3.0;
    const float twothird = 2.0 / 3.0;
    float hue = col.x;
    float sat = col.y;
    float lum = col.z;
    vec3 xt;
    if (hue < onethird)
        xt = vec3(6.0 * (onethird - hue), 6.0 * hue, 0.0);
    else if (hue < twothird)
        xt = vec3(0.0, 6.0 * (twothird - hue), 6.0 * (hue - onethird));
    else
        xt = vec3(6.0 * (hue - twothird), 0.0, 6.0 * (1.0 - hue));
    xt = min(xt, 1.0);
    vec3 ct = (2.0 * sat * xt) + (1.0 - sat);
    if (lum >= 0.5)
        return ((1.0 - lum) * ct) + ((2.0 * lum) - 1.0);
    return lum * ct;
}

void main() {
    vec2 at = v_pos * 2.0;
    vec2 r = vec2(u[0].z);
    vec4 sum = texture(tex, at * u[0].xy) * 4.0;
    sum += texture(tex, (at - r) * u[0].xy);
    sum += texture(tex, (at + r) * u[0].xy);
    sum += texture(tex, (at + vec2(r.x, -r.y)) * u[0].xy);
    sum += texture(tex, (at - vec2(r.x, -r.y)) * u[0].xy);
    vec4 blurred = sum / 8.0;
    if (u[1].x != 0.0) {
        float darkness = u[1].y;
        vec3 hsl = rgb2hsl(blurred.rgb);
        float perceived = doubleCircleSigmoid(
            sqrt(blurred.r * blurred.r * Pr + blurred.g * blurred.g * Pg + blurred.b * blurred.b * Pb),
            0.8 * darkness);
        float b1 = B * darkness;
        float base = hsl[1] > 0.0
            ? smoothstep(b1 - C * 0.5, b1 + C * 0.5,
                         1.0 - (pow(1.0 - hsl[1] * cos(A), 2.0) + pow(1.0 - perceived * sin(A), 2.0)))
            : 0.0;
        float saturation = clamp(hsl[1] + base * u[1].x, 0.0, 1.0);
        blurred.rgb = hsl2rgb(vec3(hsl[0], saturation, hsl[2]));
    }
    color = blurred;
}
