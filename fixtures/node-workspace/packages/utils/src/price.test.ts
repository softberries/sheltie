import { describe, expect, it } from "vitest";
import { applyDiscount, formatPrice } from "./price";

describe("formatPrice", () => {
  it("formats minor units", () => {
    expect(formatPrice(1999, "EUR")).toBe("19.99 EUR");
  });
});

describe("applyDiscount", () => {
  it("rounds half away from zero", () => {
    expect(applyDiscount(999, 10)).toBe(899);
  });
});
