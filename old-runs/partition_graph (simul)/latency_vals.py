
lines = []
#file_name = '15kload-2node-partition-autobahn'
#offset = 1
file_name = 'bullshark-2node-partition-15kload-5-cap-only-yourself'
offset = 2
latencies = []
with open(file_name + '-processed.txt', 'r') as f:
    lines = f.read().splitlines()

    for line in lines:
        tmp = line.split(',')
        start = float(tmp[0]) + offset
        end = float(tmp[1]) + offset
        latency = float(tmp[2])
        #print(start)
        if start > 35:
            latencies.append(latency)

#print(latencies)
mean_lat = sum(latencies) / len(latencies)
print(mean_lat)
        