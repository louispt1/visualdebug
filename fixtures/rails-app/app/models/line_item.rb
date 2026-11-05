class LineItem < ApplicationRecord
  belongs_to :order

  def subtotal
    quantity * unit_price
  end
end
