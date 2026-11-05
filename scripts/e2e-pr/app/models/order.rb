class Order < ApplicationRecord
  belongs_to :customer, optional: true
  has_many :line_items, dependent: :destroy

  def guest?
    customer.nil?
  end
end
