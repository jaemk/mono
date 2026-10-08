import { test } from "node:test";
import assert from "node:assert/strict";
import { JSDOM } from "jsdom";
import { komino } from "../helpers.mjs";

const { face, back, cardLabel } = komino;
const parse = (svg) => new JSDOM(`<body>${svg}</body>`).window.document.querySelector("svg");

test("both corners show the value, the bottom one turned about the card center", () => {
  for (let v = -1; v <= 13; v++) {
    const svg = parse(face(v));
    assert.equal(svg.getAttribute("viewBox"), "0 0 70 100");
    const corners = [...svg.querySelectorAll("text.corner")];
    assert.equal(corners.length, 2, `value ${v}`);
    const [top, bottom] = corners;
    assert.equal(top.textContent, String(v));
    assert.equal(bottom.textContent, String(v));
    // same anchor point, so the turned copy lands as far inside the opposite
    // corner as the top one is inside its own
    for (const attr of ["x", "y", "font-size"]) assert.equal(bottom.getAttribute(attr), top.getAttribute(attr));
    assert.equal(top.getAttribute("transform"), null);
    assert.equal(bottom.getAttribute("transform"), "rotate(180 35 50)");
    // the baseline sits a cap height inside the top edge, so neither copy clips
    const y = Number(top.getAttribute("y"));
    const size = Number(top.getAttribute("font-size"));
    assert.ok(y - size * 0.75 >= 1 && y <= 50);
    assert.ok(Number(top.getAttribute("x")) >= 4);
  }
});

test("plain cards show the value in the center on a value tint", () => {
  const low = parse(face(0));
  assert.equal(low.querySelector("rect").getAttribute("fill"), "var(--card-low)");
  const mid = parse(face(-1));
  assert.equal(mid.querySelector("rect").getAttribute("fill"), "var(--card-low)");
  const high = parse(face(6));
  assert.equal(high.querySelector("rect").getAttribute("fill"), "var(--card)");
  const center = [...high.querySelectorAll("text")].find((t) => !t.classList.contains("corner"));
  assert.equal(center.textContent, "6");
  assert.equal(center.getAttribute("text-anchor"), "middle");
});

test("special cards replace the center value with a colored symbol", () => {
  const colors = { 7: "#2f6fd6", 8: "#2f6fd6", 9: "#e07a1f", 10: "#e07a1f", 11: "#7b4bc9", 12: "#7b4bc9", 13: "#d0393b" };
  for (const [v, color] of Object.entries(colors)) {
    const svg = parse(face(Number(v)));
    const texts = [...svg.querySelectorAll("text")];
    assert.equal(texts.length, 2, "only the corners carry text");
    assert.equal(svg.querySelector("rect").getAttribute("stroke"), color);
    assert.ok(svg.querySelectorAll("path, circle, polygon, line").length > 0);
  }
  // each move has its own symbol
  const shapes = [7, 9, 11, 13].map((v) => parse(face(v)).innerHTML.replace(/#[0-9a-f]{6}/g, ""));
  assert.equal(new Set(shapes).size, 4);
});

test("card backs are one shared design", () => {
  assert.equal(back(), back());
  assert.ok(parse(back()).querySelector("path"));
  // the slot number sits at the center (UI-31); nothing there may cover it
  assert.equal(parse(back()).querySelector("circle"), null);
});

test("labels name the move", () => {
  assert.equal(cardLabel(4), "4");
  assert.equal(cardLabel(-1), "-1");
  assert.equal(cardLabel(7), "7, peek own");
  assert.equal(cardLabel(10), "10, peek other");
  assert.equal(cardLabel(12), "12, blind swap");
  assert.equal(cardLabel(13), "13, look and swap");
});
