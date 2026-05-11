using ArgParse
include("../Experiments/Experiments.jl")

mix_scheme = [(Degree, 8), (QuasiStable, 8), (NeighborNodeLabels, 8), (NodeLabels, 8)]

parser = ArgParseSettings()
@add_arg_table! parser begin
    "-s", "--summary"
    help = "Path to the summary file (.obj)"
    required = true

    "-q", "--query"
    help = "Path to the query file (in G-CARE) format"
    required = true

    "-r", "--replications"
    help = "Number of replications"
    default = 3
end
args = parse_args(parser)

s::ColorSummary = deserialize(args["summary"])
q = load_query(args["query"], subgraph_matching_data=false)
params = ExperimentParams(deg_stats_type=MaxDegStats,
    dataset=aids, # placeholder
    partitioning_scheme=mix_scheme,
    description="COLOR \n(MaxMix32)")
results = [(@timed get_cardinality_bounds(q, s;
    max_partial_paths=params.inference_max_paths,
    use_partial_sums=params.use_partial_sums,
    usingStoredStats=true,
    sampling_strategy=params.sampling_strategy,
    only_shortest_path_cycle=params.only_shortest_path_cycle,
    timeout=300.0)) for _ in 1:args["replications"]]
estimate_time = median([x.time for x in results])
estimate = max(1, results[1].value)
if isinf(estimate)
    estimate = 10^35
end
if isnan(estimate)
    estimate = 1.0
end
println("$(estimate),$(estimate_time)")