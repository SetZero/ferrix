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

  // `series`: [{ label, color, points: [[t, value], ...] }]
  draw(series, now) {
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
    const min = this.min;
    const x = (t) => left + ((t - start) / this.span) * plotW;
    const y = (v) => top + plotH - ((v - min) / (max - min || 1)) * plotH;

    // Gridlines and their labels.
    g.font = "11px Inter, Roboto, sans-serif";
    g.textAlign = "right";
    g.textBaseline = "middle";
    for (let i = 0; i <= 4; i++) {
      const v = min + ((max - min) * i) / 4;
      const yy = Math.round(y(v)) + 0.5;
      g.strokeStyle = i === 0 ? "#34344a" : "#23232f";
      g.beginPath(); g.moveTo(left, yy); g.lineTo(width - right, yy); g.stroke();
      g.fillStyle = "#77758a";
      g.fillText(format(v, this.decimals > 0 && max - min < 10 ? 1 : 0), left - 6, yy);
    }
    g.textAlign = "left";
    g.fillStyle = "#77758a";
    g.fillText(`−${Math.round(this.span / 60)} min`, left, height - 6);

    // The series.
    series.forEach((s, index) => {
      const points = s.points.filter(([t]) => t >= start - 2);
      if (points.length < 2) return;
      g.lineWidth = 1.8;
      g.strokeStyle = s.color;
      g.beginPath();
      points.forEach(([t, v], i) => (i ? g.lineTo(x(t), y(v)) : g.moveTo(x(t), y(v))));
      g.stroke();
      if (index === 0) {
        const fill = g.createLinearGradient(0, top, 0, top + plotH);
        fill.addColorStop(0, s.color + "40");
        fill.addColorStop(1, s.color + "00");
        g.lineTo(x(points[points.length - 1][0]), top + plotH);
        g.lineTo(x(points[0][0]), top + plotH);
        g.closePath();
        g.fillStyle = fill;
        g.fill();
      }
    });

    if (!series.some((s) => s.points.some(([t]) => t >= start))) {
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
