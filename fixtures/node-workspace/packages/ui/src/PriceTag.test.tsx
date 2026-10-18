import { describe, expect, it } from "vitest";
import { PriceTag } from "./PriceTag";

describe("PriceTag", () => {
  it("renders", () => {
    expect(PriceTag({ price: { cents: 100, currency: "EUR" } })).toBeTruthy();
  });
});
