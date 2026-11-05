class Pricer
  TIER_DISCOUNTS = { "gold" => 0.1, "silver" => 0.05 }.freeze
  BULK_THRESHOLD = 10
  BULK_DISCOUNT = 0.05

  def initialize(order)
    @order = order
  end

  def call
    tier_discount = TIER_DISCOUNTS[@order.customer&.tier] || 0
    subtotal = 0
    units = 0
    @order.line_items.each do |item|
      subtotal += item.subtotal
      units += item.quantity
    end
    bulk_discount = units >= BULK_THRESHOLD ? BULK_DISCOUNT : 0
    discount = [tier_discount, bulk_discount].max
    total = (subtotal * (1 - discount)).round(2)
    @order.update!(total: total)
    total
  end
end
