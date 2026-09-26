// A small line chart on a canvas: time on x, the last `span` seconds; series
// drawn as smooth lines with a faint fill under the first, a legend with each
// series' latest value, and gridlines at round numbers.

"use strict";

class LineChart {
  constructor(canvas, { min = 0, max = null, unit = "", span = 300, decimals = 0 } = {}) {
    this.canvas = canvas;
    this.min = min;
    this.max = max;
    this.unit = unit;
    this.span = span;
    this.decimals = decimals;
  }

  // `series`: [{ label, color, points: [[t, value], ...] }]. `bands`:
  // [{ from, to, label, color }], spans shaded behind the lines, for where the
  // phone was away. A gap of more than `gap` seconds between two points is
  // left as a gap rather than drawn across.
  draw(series, now, bands = [], gap = 3) {
    const canvas = this.canvas;
    const ratio = window.devicePixelRatio || 1;
    const width = canvas.clientWidth;
    const height = canvas.clientHeight;
    if (width === 0 || height === 0) return;
    if (canvas.width !== Math.round(width * ratio) || canvas.height !== Math.round(height * ratio)) {
      canvas.width = Math.round(width * ratio);
      canvas.height = Math.round(height * ratio);
    }
    const g = canvas.getContext("2d");
    g.setTransform(ratio, 0, 0, ratio, 0, 0);
    g.clearRect(0, 0, width, height);

    const legendHeight = 18;
    const left = 40, right = 8, top = legendHeight + 6, bottom = 16;
    const plotW = width - left - right, plotH = height - top - bottom;
    const start = now - this.span;

    let max = this.max;
    if (max === null) {
      max = 0;
      for (const s of series) for (const [t, v] of s.points) if (t >= start && v > max) max = v;
      max = niceCeiling(max || 1);
    }
    // A floor of `null` follows the data down, for a value that can go
    // negative, such as a battery's draw while it charges.
    let min = this.min;
    if (min === null) {
      min = 0;
      for (const s of series) for (const [t, v] of s.points) if (t >= start && v < min) min = v;
      if (min < 0) min = -niceCeiling(-min);
    }
    const x = (t) => left + ((t - start) / this.span) * plotW;
    const y = (v) => top + plotH - ((v - min) / (max - min || 1)) * plotH;

    const hasData = series.some((s) => s.points.some(([t]) => t >= start));

    // Gridlines, and their labels only when there is something to read
    // against them: an empty chart's scale is not a value.
    g.font = "11px Inter, Roboto, sans-serif";
    g.textAlign = "right";
    g.textBaseline = "middle";
    for (let i = 0; i <= 4; i++) {
      const v = min + ((max - min) * i) / 4;
      const yy = Math.round(y(v)) + 0.5;
      g.strokeStyle = i === 0 ? "#34344a" : "#23232f";
      g.beginPath(); g.moveTo(left, yy); g.lineTo(width - right, yy); g.stroke();
      g.fillStyle = "#77758a";
      if (hasData) g.fillText(format(v, this.decimals > 0 && max - min < 10 ? 1 : 0), left - 6, yy);
    }
    g.textAlign = "left";
    g.fillStyle = "#77758a";
    g.fillText(`−${Math.round(this.span / 60)} min`, left, height - 6);

    // The bands, behind everything but the grid.
    g.save();
    g.beginPath();
    g.rect(left, top, plotW, plotH);
    g.clip();
    for (const band of bands) {
      const from = Math.max(x(band.from), left);
      const to = Math.min(x(band.to), left + plotW);
      if (to <= from) continue;
      g.fillStyle = band.color + "22";
      g.fillRect(from, top, to - from, plotH);
      // Dashed edges rather than a stripe along the top, which would read
      // as a line at the top of the scale.
      g.strokeStyle = band.color + "99";
      g.lineWidth = 1;
      g.setLineDash([3, 3]);
      g.beginPath();
      for (const edge of [from, to]) {
        if (edge > left + 0.5 && edge < left + plotW - 0.5) {
          g.moveTo(Math.round(edge) + 0.5, top);
          g.lineTo(Math.round(edge) + 0.5, top + plotH);
        }
      }
      g.stroke();
      g.setLineDash([]);
      if (to - from > 34) {
        g.font = "10px Inter, Roboto, sans-serif";
        g.textAlign = "left";
        g.textBaseline = "top";
        g.fillStyle = band.color;
        g.fillText(band.label, from + 4, top + 4, to - from - 8);
      }
    }
    g.restore();

    // The series, a run of points at a time: a gap stays a gap.
    series.forEach((s, index) => {
      const points = s.points.filter(([t]) => t >= start - 2);
      const runs = [];
      for (const point of points) {
        const run = runs[runs.length - 1];
        if (run && point[0] - run[run.length - 1][0] <= gap) run.push(point);
        else runs.push([point]);
      }
      for (const run of runs) {
        if (run.length < 2) continue;
        g.lineWidth = 1.8;
        g.strokeStyle = s.color;
        g.beginPath();
        run.forEach(([t, v], i) => (i ? g.lineTo(x(t), y(v)) : g.moveTo(x(t), y(v))));
        g.stroke();
        if (index === 0) {
          const fill = g.createLinearGradient(0, top, 0, top + plotH);
          fill.addColorStop(0, s.color + "40");
          fill.addColorStop(1, s.color + "00");
          g.lineTo(x(run[run.length - 1][0]), top + plotH);
          g.lineTo(x(run[0][0]), top + plotH);
          g.closePath();
          g.fillStyle = fill;
          g.fill();
        }
      }
    });

    if (!hasData) {
      g.fillStyle = "#77758a";
      g.textAlign = "center";
      g.textBaseline = "middle";
      g.fillText("no data yet", left + plotW / 2, top + plotH / 2);
      return;
    }

    // The legend, latest values, with the unit only where it is one symbol:
    // the card's heading gives the rest.
    let lx = left;
    g.textBaseline = "top";
    const unit = this.unit.trim().length <= 1 ? this.unit : "";
    for (const s of series) {
      const last = s.points.length ? s.points[s.points.length - 1][1] : null;
      if (last === null) continue;
      const text = `${s.label} ${format(last, this.decimals) + unit}`;
      g.fillStyle = s.color;
      g.fillRect(lx, 5, 8, 8);
      g.fillStyle = "#c9c6d6";
      g.fillText(text, lx + 12, 2);
      lx += g.measureText(text).width + 26;
      if (lx > width - 40) break;
    }
  }
}

function niceCeiling(value) {
  const exponent = Math.pow(10, Math.floor(Math.log10(value)));
  for (const step of [1, 2, 2.5, 5, 10]) if (step * exponent >= value) return step * exponent;
  return 10 * exponent;
}

function format(value, decimals) {
  return Number(value).toFixed(decimals);
}
