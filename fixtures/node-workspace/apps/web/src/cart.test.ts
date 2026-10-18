import { expect, it } from "vitest";
import { cartTotal } from "./cart";

it("sums", () => {
  expect(cartTotal([{ cents: 1, currency: "EUR" }]).cents).toBe(1);
});
