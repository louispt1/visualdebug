require "rails_helper"

RSpec.describe Pricer do
  it "applies the gold tier discount" do
    customer = Customer.create!(name: "Ada", tier: "gold")
    order = Order.create!(customer: customer, total: 0)
    order.line_items.create!(sku: "A", quantity: 2, unit_price: 50)
    order.line_items.create!(sku: "B", quantity: 1, unit_price: 20)

    expect(Pricer.new(order).call).to eq(108)
  end

  it "prices guest orders without a discount" do
    order = Order.create!(customer: nil, total: 0)
    order.line_items.create!(sku: "A", quantity: 3, unit_price: 10)

    expect(Pricer.new(order).call).to eq(30)
  end

  it "gives guests the bulk discount from 10 units" do
    order = Order.create!(customer: nil, total: 0)
    order.line_items.create!(sku: "A", quantity: 10, unit_price: 10)

    expect(Pricer.new(order).call).to eq(95)
  end

  it "keeps the larger of tier and bulk discounts" do
    customer = Customer.create!(name: "Sam", tier: "silver")
    order = Order.create!(customer: customer, total: 0)
    order.line_items.create!(sku: "A", quantity: 12, unit_price: 5)

    expect(Pricer.new(order).call).to eq(57)
  end
end
