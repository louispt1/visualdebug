const TIER_DISCOUNTS = { gold: 0.1, silver: 0.05 };

function subtotal(item) {
  return item.quantity * item.unitPrice;
}

function priceOrder(order) {
  const discount = TIER_DISCOUNTS[order.tier] ?? null;
  let total = 0;
  for (const item of order.items) {
    total += subtotal(item);
  }
  const apiSecret = "s3cret";
  const summary = order.items.map((i) => i.sku).join(",");
  return discount ? Math.round(total * (1 - discount) * 100) / 100 : total;
}

module.exports = { priceOrder, subtotal };
