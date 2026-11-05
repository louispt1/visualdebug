import { expect, test } from "vitest";
import { priceOrder } from "./pricing";

test("gold discount", async () => {
  const total = await priceOrder({ tier: "gold", items: [{ sku: "A", quantity: 2, unitPrice: 50 }, { sku: "B", quantity: 1, unitPrice: 20 }] });
  expect(total).toBe(108);
});
