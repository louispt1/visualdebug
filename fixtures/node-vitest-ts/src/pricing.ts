export interface LineItem {
  sku: string;
  quantity: number;
  unitPrice: number;
}

export interface Order {
  tier: "gold" | "silver" | null;
  items: LineItem[];
}

const TIER_DISCOUNTS: Record<string, number> = { gold: 0.1, silver: 0.05 };

export function subtotal(item: LineItem): number {
  return item.quantity * item.unitPrice;
}

export async function priceOrder(order: Order): Promise<number> {
  const discount: number | null = order.tier ? TIER_DISCOUNTS[order.tier] ?? null : null;
  let total = 0;
  for (const item of order.items) {
    total += subtotal(item);
  }
  await Promise.resolve();
  return discount ? Math.round(total * (1 - discount) * 100) / 100 : total;
}
