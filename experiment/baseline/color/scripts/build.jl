using ArgParse
include("../Experiments/Experiments.jl")

mix_scheme = [(Degree, 8), (QuasiStable, 8), (NeighborNodeLabels, 8), (NodeLabels, 8)]

s = ArgParseSettings()
@add_arg_table! s begin
    "-d", "--dataset"
    help = "Path to the dataset file (in G-CARE format)"
    required = true

    "-o", "--output"
    help = "Path to the output summary file"
    required = true
end
args = parse_args(s)

data = load_dataset(args["dataset"], subgraph_matching_data=false)
params = ColorSummaryParams(deg_stats_type=AvgDegStats,
    max_cycle_size=6,
    max_partial_paths=50000,
    partitioning_scheme=mix_scheme,
    weighting=true,
    proportion_updated=0.0,
    proportion_deleted=0.0)
timing_vec = Float64[]
results = @timed generate_color_summary(data, params; verbose=0, timing_vec=timing_vec)
current_summary = results.value
summary_size = Base.summarysize(current_summary)
serialize_results = @timed serialize(args["output"], current_summary)
println("FullTime: $(results.time) s")
println("Coloring: $(timing_vec[1]) s")
println("CycleCounting: $(timing_vec[2]) s")
println("BloomFilter: $(timing_vec[3]) s")
println("CardinalityCounting: $(timing_vec[4]) s")
println("EdgeStats: $(timing_vec[5]) s")
println("SummarySize: $(summary_size) bytes")
println("SerializeTime: $(serialize_results.time) s")