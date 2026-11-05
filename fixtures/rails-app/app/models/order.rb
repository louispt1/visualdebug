class Order < ApplicationRecord
  belongs_to :customer
  has_many :line_items, dependent: :destroy
end
