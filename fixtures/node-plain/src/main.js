const { priceOrder } = require("./pricing");
const order = { tier: "gold", items: [{ sku: "A", quantity: 2, unitPrice: 50 }, { sku: "B", quantity: 1, unitPrice: 20 }] };
console.log("total", priceOrder(order));
