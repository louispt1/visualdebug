# Run: ruby probes/ruby/test/probe_test.rb
require "minitest/autorun"
require "json"
require "tmpdir"
require "open3"
require "rbconfig"

class ProbeTest < Minitest::Test
  PROBE = File.expand_path("../visualdebug_probe.rb", __dir__)

  # Runs `code` (written to <root>/app.rb) under the probe; returns [events, stdout, status, errors].
  def trace(code, start_at: nil, line_files: nil, max_events: 1000, lib: nil, limits: nil)
    Dir.mktmpdir do |dir|
      root = File.join(dir, "proj")
      out = File.join(root, ".visualdebug", "run")
      FileUtils.mkdir_p(out)
      if lib
        FileUtils.mkdir_p(File.join(dir, "gems"))
        File.write(File.join(dir, "gems", "lib.rb"), lib)
      end
      File.write(File.join(root, "app.rb"), code)
      cfg = { root: root, out_dir: out, start_at: start_at, line_files: line_files, max_events: max_events, limits: limits }
      File.write(File.join(out, "config.json"), JSON.generate(cfg))
      env = { "VISUALDEBUG_CONFIG" => File.join(out, "config.json"), "RUBYOPT" => "-r#{PROBE}" }
      stdout, status = Open3.capture2e(env, RbConfig.ruby, File.join(root, "app.rb"), chdir: root)
      events = Dir[File.join(out, "trace-*.jsonl")].flat_map { |f| File.readlines(f).map { |l| JSON.parse(l) } }
      errors = File.exist?(File.join(out, "probe-errors.log")) ? File.read(File.join(out, "probe-errors.log")) : ""
      return [events.reject { |e| e["event"] == "meta" }, stdout, status, errors, events]
    end
  end

  def test_program_output_and_exit_status_unchanged
    _, out, status = trace("def f(x) = x * 2\nputs f(21)\nexit 3\n")
    assert_equal "42\n", out
    assert_equal 3, status.exitstatus
  end

  def test_exceptions_still_propagate_and_are_recorded
    events, out, status = trace("def boom = raise(ArgumentError, 'nope')\nbegin\n  boom\nrescue => e\n  puts e.message\nend\n")
    assert_equal "nope\n", out
    assert status.success?
    raise_ev = events.find { |e| e["event"] == "raise" }
    assert_equal "ArgumentError", raise_ev["exception"]["type"]
  end

  def test_never_calls_library_defined_inspect
    lib = "class Lazy\n  def initialize = @n = 1\n  def inspect\n    File.write(ENV.fetch('MARK'), 'called')\n    'x'\n  end\nend\n"
    Dir.mktmpdir do |d|
      mark = File.join(d, "mark")
      ENV["MARK"] = mark
      events, = trace("require_relative '../gems/lib'\nobj = Lazy.new\nputs 1\n", lib: lib)
      refute File.exist?(mark), "library #inspect must not run"
      line = events.select { |e| e["event"] == "line" }.last
      assert_equal "#<Lazy @n=1>", line["locals"]["obj"]["repr"]
    end
  end

  def test_redaction_follows_the_value
    events, = trace("def login(password)\n  pw = password\n  pw\nend\nlogin('hunter2')\nh = { api_key: 'k', name: 'n' }\nputs h.size\n")
    dump = JSON.generate(events)
    refute_includes dump, "hunter2"
    refute_includes dump, "\"k\""
    assert_includes dump, "[REDACTED]"
  end

  def test_truncation_and_limits
    events, = trace("big = (1..60).to_a\ns = 'x' * 5000\nputs big.size\n")
    locals = events.select { |e| e["event"] == "line" }.last["locals"]
    assert locals["big"]["truncated"]
    assert_includes locals["big"]["repr"], "10 more"
    assert locals["s"]["truncated"]
    assert_operator locals["s"]["repr"].length, :>, 1000
    assert_operator locals["s"]["repr"].length, :<=, 2005
    events, = trace("s = 'x' * 500\nputs s.size\n", limits: { repr: 100 })
    s = events.select { |e| e["event"] == "line" }.last["locals"]["s"]
    assert s["truncated"]
    assert_operator s["repr"].length, :<=, 105
  end

  def test_max_events_marks_truncated
    _, _, _, _, all = trace("i = 0\nwhile i < 1000\n  i += 1\nend\n", max_events: 50)
    assert all.any? { |e| e["kind"] == "truncated" }
    assert_equal 50, all.count { |e| e["event"] != "meta" }
  end

  def test_retry_does_not_corrupt_the_stack
    code = <<~RUBY
      def flaky
        @n = (@n || 0) + 1
        raise "again" if @n < 3
        :ok
      end
      def outer
        begin
          flaky
        rescue
          retry
        end
      end
      def after = :after
      outer
      after
    RUBY
    events, = trace(code)
    after = events.find { |e| e["event"] == "call" && e["function"] == "Object#after" }
    assert_equal 2, after["depth"], "a frame leaked: #{after.inspect}" # <main> is depth 1
  end

  def test_start_at_file_line_and_singleton
    code = "class Calc\n  def self.sq(x)\n    y = x * x\n    y\n  end\nend\nCalc.sq(2)\nCalc.sq(3)\n"
    events, = trace(code, start_at: "app.rb:3")
    calls = events.select { |e| e["event"] == "call" }
    assert_equal ["Calc.sq", "Calc.sq"], calls.map { |e| e["function"] }
    assert_equal [1, 1], calls.map { |e| e["depth"] }
    events, = trace(code, start_at: "Calc.sq")
    assert_equal 2, events.count { |e| e["event"] == "call" }
  end

  def test_hook_errors_are_swallowed
    # A start_at that never resolves plus an object whose class lookup is hostile.
    code = "class Evil < BasicObject\n  def method_missing(*) = ::Kernel.raise('boom')\nend\ne = Evil.new\n$stdout.puts 'fine'\n"
    _, out, status = trace(code)
    assert_equal "fine\n", out
    assert status.success?
  end
end
