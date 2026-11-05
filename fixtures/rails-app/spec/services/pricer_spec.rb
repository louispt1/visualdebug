require "rails_helper"

RSpec.describe Pricer do
  it "applies the gold tier discount" do
    customer = Customer.create!(name: "Ada", tier: "gold")
    order = Order.create!(customer: customer, total: 0)
    order.line_items.create!(sku: "A", quantity: 2, unit_price: 50)
    order.line_items.create!(sku: "B", quantity: 1, unit_price: 20)

    expect(Pricer.new(order).call).to eq(108)
  end
end
