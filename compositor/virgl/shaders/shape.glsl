// Whether the pixel whose centre is `p` is inside `rect` (x, y, width,
// height) with its corners cut to `radius` by a superellipse of `power`:
// `rounding.glsl`'s curve, all or nothing, as compositor/render's
// `corner_inset` cuts it.
float inside(vec2 p, vec4 rect, float radius, float power) {
    vec2 lo = rect.xy + radius;
    vec2 hi = rect.xy + rect.zw - radius;
    vec2 d = max(max(lo - p, p - hi), 0.0);
    if (radius <= 0.0 || d.x <= 0.0 || d.y <= 0.0)
        return 1.0;
    return (pow(d.x, power) + pow(d.y, power) < pow(radius, power)) ? 1.0 : 0.0;
}
