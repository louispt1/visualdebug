require "rails_helper"

RSpec.describe LineItem do
  it "computes subtotal" do
    expect(LineItem.new(quantity: 3, unit_price: 2.5).subtotal).to eq(7.5)
  end
end
