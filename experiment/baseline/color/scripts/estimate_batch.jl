using ArgParse
using Printf
using Statistics
include("../Experiments/Experiments.jl")

mix_scheme = [(Degree, 8), (QuasiStable, 8), (NeighborNodeLabels, 8), (NodeLabels, 8)]

parser = ArgParseSettings()
@add_arg_table! parser begin
    "-s", "--summary"
    help = "Path to the summary file (.obj)"
    required = true

    "-f", "--queries-file"
    help = "Text file with one G-CARE query path per line"
    required = true

    "-o", "--output"
    help = "Output CSV path"
    required = true

    "-r", "--replications"
    help = "Number of replications"
    arg_type = Int
    default = 3
end
args = parse_args(parser)

s::ColorSummary = deserialize(args["summary"])
params = ExperimentParams(deg_stats_type=AvgDegStats,
    dataset=aids, # placeholder
    partitioning_scheme=mix_scheme,
    description="COLOR \n(AvgMix32)")

function clean_error(err)
    text = replace(string(err), '\n' => ' ', ',' => ' ')
    return text[1:min(end, 500)]
end

queries = filter(!isempty, strip.(readlines(args["queries-file"])))
open(args["output"], "w") do io
    println(io, "query,estimate,latency_s,status,error")
    flush(io)
    for query_path in queries
        query_name = splitext(basename(query_path))[1]
        try
            q = load_query(query_path, subgraph_matching_data=false)
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
            @printf(io, "%s,%.17g,%.6f,ok,\n", query_name, estimate, estimate_time)
        catch err
            @printf(io, "%s,,0.000000,failed,%s\n", query_name, clean_error(err))
        end
        flush(io)
    end
end
