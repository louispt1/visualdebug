import { priceOrder } from "./pricing";

const total = await priceOrder({ tier: "gold", items: [{ sku: "A", quantity: 2, unitPrice: 50 }] });
console.log("total", total);
