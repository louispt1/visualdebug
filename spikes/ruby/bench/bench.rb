class Bench
  def run(n)
    acc = 0
    items = (1..20).to_a
    n.times do |i|
      acc += helper(i, items)
    end
    acc
  end

  def helper(i, items)
    x = i * 2
    x + items.size
  end
end
t = Process.clock_gettime(Process::CLOCK_MONOTONIC)
Bench.new.run(3000)
el = Process.clock_gettime(Process::CLOCK_MONOTONIC) - t
puts "run: #{(el * 1000).round(2)}ms"
