import numpy as np
import matplotlib.pyplot as plt
import pandas as pd


def adjust_file(file_name, offset):
    lines = []
    with open(file_name + '.txt') as f:
        lines = f.read().splitlines()

    with open(file_name + '-processed.txt', 'w') as f:
        for line in lines:
            tmp = line.split(',')
            start = float(tmp[0]) + offset
            end = float(tmp[1]) + offset
            latency = float(tmp[2])
            f.write(f"{start},{end},{latency}\n")
        


hs_file_name = '2node-partition-20s-15kload'
motorway_file_name = '75kload-partition-updated-figure'
motorway_file_name2 = '15kload-2node-partition-autobahn'
#motorway_file_name = '2node-partition-20s-220kload-1stimeout'
bullshark_file_name = 'bullshark-220kload-2node-partition'
batch_hs_file_name = 'batched-hs-2node-partition-100kload-20s-1stimeout'
output_file_name = '2node-partition.pdf'

async_start = 7.5
async_end = 28.5
hangover_end = 49
hangover_end2 = 33

hs_start_cutoff = 5
hs_end_cutoff = 8

motor_start_cutoff = 4
motor_end_cutoff = 9
motor_offset = 1

motor_start_cutoff2 = 4
motor_end_cutoff2 = 9
motor_offset2 = 1

bullshark_start_cutoff = 2
bullshark_end_cutoff = 5
bullshark_offset = 1.3

batch_hs_start_cutoff = 4
batch_hs_end_cutoff = 3
batch_hs_offset = -0.8

adjust_file(motorway_file_name, motor_offset)
adjust_file(motorway_file_name2, motor_offset2)
adjust_file(bullshark_file_name, bullshark_offset)
adjust_file(batch_hs_file_name, batch_hs_offset)

df_hs = pd.read_csv(hs_file_name + '.txt', sep=',', header=None)
df_hs.columns = ["start", "end", "latency"]
df_hs['start'] = pd.to_timedelta(df_hs['start'], unit='s')
#df['end'] = pd.to_timedelta(df['end'], unit='s')
plot_df_hs = df_hs.groupby(pd.Grouper(key='start', freq='1s')).mean()
#print(plot_df.index.get_level_values('start'))
x = plot_df_hs.index.get_level_values('start') / np.timedelta64(1, 's')
x = x.to_list()[hs_start_cutoff:-hs_end_cutoff]
y = plot_df_hs.get('latency').to_list()[hs_start_cutoff:-hs_end_cutoff]

plt.figure(figsize=(6.4,3.9))

plt.scatter(np.array(x), np.array(y), s=10, color='red', label='Vanilla HS (15k)',marker='v')
plt.plot(x, y, '.r-')


df_motor = pd.read_csv(motorway_file_name2 + '-processed.txt', sep=',', header=None)
df_motor.columns = ["start", "end", "latency"]
df_motor['start'] = pd.to_timedelta(df_motor['start'], unit='s')
#df['end'] = pd.to_timedelta(df['end'], unit='s')
plot_df_motor = df_motor.groupby(pd.Grouper(key='start', freq='1s')).mean()
#print(plot_df.index.get_level_values('start'))
x_motor = plot_df_motor.index.get_level_values('start') / np.timedelta64(1, 's')
x_motor = x_motor.to_list()[motor_start_cutoff:-motor_end_cutoff]
y_motor = plot_df_motor.get('latency').to_list()[motor_start_cutoff:-motor_end_cutoff]

plt.scatter(np.array(x_motor), np.array(y_motor), s=20, color='green', label='Autobahn (15k)', marker='x')
plt.plot(x_motor, y_motor, '.g-')

df_motor = pd.read_csv(motorway_file_name + '-processed.txt', sep=',', header=None)
df_motor.columns = ["start", "end", "latency"]
df_motor['start'] = pd.to_timedelta(df_motor['start'], unit='s')
#df['end'] = pd.to_timedelta(df['end'], unit='s')
plot_df_motor = df_motor.groupby(pd.Grouper(key='start', freq='1s')).mean()
#print(plot_df.index.get_level_values('start'))
x_motor = plot_df_motor.index.get_level_values('start') / np.timedelta64(1, 's')
x_motor = x_motor.to_list()[motor_start_cutoff:-motor_end_cutoff]
y_motor = plot_df_motor.get('latency').to_list()[motor_start_cutoff:-motor_end_cutoff]

plt.scatter(np.array(x_motor), np.array(y_motor), s=10, color='green', label='Autobahn (75k)')
plt.plot(x_motor, y_motor, '.g--')




df_bs = pd.read_csv(bullshark_file_name + '-processed.txt', sep=',', header=None)
df_bs.columns = ["start", "end", "latency"]
df_bs['start'] = pd.to_timedelta(df_bs['start'], unit='s')
#df['end'] = pd.to_timedelta(df['end'], unit='s')
plot_df_bs = df_bs.groupby(pd.Grouper(key='start', freq='1s')).mean()
#print(plot_df.index.get_level_values('start'))
x_bs = plot_df_bs.index.get_level_values('start') / np.timedelta64(1, 's')
x_bs = x_bs.to_list()[bullshark_start_cutoff:-bullshark_end_cutoff]
y_bs = plot_df_bs.get('latency').to_list()[bullshark_start_cutoff:-bullshark_end_cutoff]

#plt.scatter(np.array(x_bs), np.array(y_bs), s=10, color='blue', label='Bullshark')
#plt.plot(x_bs, y_bs, '.b-')

df_batch = pd.read_csv(batch_hs_file_name + '-processed.txt', sep=',', header=None)
df_batch.columns = ["start", "end", "latency"]
df_batch['start'] = pd.to_timedelta(df_batch['start'], unit='s')
#df['end'] = pd.to_timedelta(df['end'], unit='s')
plot_df_batch = df_batch.groupby(pd.Grouper(key='start', freq='1s')).mean()
#print(plot_df.index.get_level_values('start'))
x_batch = plot_df_batch.index.get_level_values('start') / np.timedelta64(1, 's')
x_batch = x_batch.to_list()[batch_hs_start_cutoff:-batch_hs_end_cutoff]
y_batch = plot_df_batch.get('latency').to_list()[batch_hs_start_cutoff:-batch_hs_end_cutoff]

#plt.scatter(np.array(x_batch), np.array(y_batch), s=10, color='purple', label='Batch HS')
#plt.plot(x_batch, y_batch, '.m-')

plt.vlines(x=[async_start,async_end], ymin=0, ymax=23, colors='orange', ls='--', lw=2.5, label='Blip Duration')
plt.vlines(x=[hangover_end], ymin=0, ymax=23, colors='blue', ls='dotted', lw=2.5, label='Hangover Duration')
plt.vlines(x=[hangover_end2], ymin=0, ymax=23, colors='blue', ls='dotted', lw=1.5)
plt.vlines(x=[29.8], ymin=0, ymax=23, colors='blue', ls='dotted', lw=1.5)

plt.xlabel('Request start time (s)', fontsize=14)
plt.ylabel('Latency (s)', fontsize=14)
plt.xticks(fontsize=14)
plt.yticks(fontsize=14)
plt.legend()
plt.tight_layout()
plt.savefig(output_file_name, format='pdf')
#plt.show()
#print(x.to_list())
#print(y)

#df.index = pd.to_datetime(df['latency'])

#df["start"] = [s+offset for s in df["start"]]
# group into bins of size 75
#n = 75
# skip every nth row, where n is the window size
# https://stackoverflow.com/questions/57595661/non-overlapping-rolling-windows-in-pandas-dataframes
#new_df = df.rolling(n).mean()[n-1::n]
#new_df = df.groupby(df.index // n).mean()
#new_df = df.resample('1s').mean().rolling('60s')
#x = new_df['start']
#x = x[plot_start:end]
#y = new_df['latency']
#print(y)
#y = y[plot_start:end]
#plt.scatter(np.array(x), np.array(y), s=10, color='red', label='Vanilla HS')

#offset = 0
#df_ab = pd.read_csv('autobahn-blips-latencies-extended.txt', sep=',', header=None)
#df_ab.columns = ["start", "end", "latency"]
#df_ab["start"] = [s+offset for s in df_ab["start"]]
# group into bins of size 75
# skip every nth row, where n is the window size
# https://stackoverflow.com/questions/57595661/non-overlapping-rolling-windows-in-pandas-dataframes
#new_df = df.rolling(n).mean()[n-1::n]
#new_df_ab = df_ab.groupby(df_ab.index // n).mean()
#x = new_df_ab['start']
#x = x[plot_start:end]
#y = new_df_ab['latency']
#y = y[plot_start:end]
#plt.scatter(np.array(x), np.array(y), s=10, color='green', label='Autobahn')

#plt.show()


#plt.xlabel('Request start time (s)', fontsize=14)
#plt.ylabel('Latency (s)', fontsize=14)
#plt.xticks(fontsize=14)
#plt.yticks(fontsize=14)

#hs_x, hs_y = process_latencies('vanilla-hs-blips-final.txt', 1.6)
#autobahn_x, autobahn_y = process_latencies('autobahn-blips-latencies-extended.txt', 0)
#print(hs_y[plot_start:end])
#plt.scatter(hs_x[plot_start:end], hs_y[plot_start:end], color='red', s=10, label='Vanilla HS')
#plt.scatter(autobahn_x[plot_start:end], autobahn_y[plot_start:end], color='green', s=10, label='Autobahn')
#plt.legend()
#plt.tight_layout()

#plt.savefig('hangover-blips-both-test.pdf', format='pdf')
