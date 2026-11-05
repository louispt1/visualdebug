class Pricer
  TIER_DISCOUNTS = { "gold" => 0.1, "silver" => 0.05 }.freeze

  def initialize(order)
    @order = order
  end

  def call
    discount = TIER_DISCOUNTS[@order.customer.tier]
    subtotal = 0
    @order.line_items.each do |item|
      subtotal += item.subtotal
    end
    total = discount ? subtotal * (1 - discount) : subtotal
    @order.update!(total: total)
    total
  end
end
