require "rails_helper"

RSpec.describe "POST /orders/:id/price", type: :request do
  it "prices the order" do
    customer = Customer.create!(name: "Ada", tier: "gold")
    order = Order.create!(customer: customer, total: 0)
    order.line_items.create!(sku: "A", quantity: 2, unit_price: 50)

    post "/orders/#{order.id}/price"

    expect(response.parsed_body["total"]).to eq(90.0)
  end
end
