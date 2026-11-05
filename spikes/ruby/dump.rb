require "json"
File.foreach(ARGV[0]) do |l|
  e = JSON.parse(l)
  loc = (e["locals"] || {}).map { |k, v| "#{k}=#{v["repr"][0, 60]}" }.join(" ")
  se = e["side_effect"]
  rv = e["return_value"]&.dig("repr")
  puts [e["seq"], "  " * (e["depth"].to_i), e["event"], e["library"] ? "(lib)" : nil, e["file"] && "#{e["file"]}:#{e["line"]}",
        e["function"], loc, se && "SQL #{se["sql"]} #{se["binds"]}", rv && "=> #{rv[0, 60]}", e["exception"]&.values&.join(": ")].compact.join(" ")
end
