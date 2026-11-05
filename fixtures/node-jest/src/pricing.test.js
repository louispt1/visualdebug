const { priceOrder } = require("./pricing");

test("gold discount", () => {
  const order = { tier: "gold", items: [{ sku: "A", quantity: 2, unitPrice: 50 }, { sku: "B", quantity: 1, unitPrice: 20 }] };
  expect(priceOrder(order)).toBe(108);
});
