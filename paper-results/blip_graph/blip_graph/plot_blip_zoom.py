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
        


hs_file_name_1 = '15kload-1fault-1stimeout-processed'
hs_file_name_2 = '15kload-1fault-exponential-1stimeout-processed'
hs_file_name_3 = '15kload-1fault-5stimeout-processed'
#
motorway_file_name_1 = '220kload-1fault-1stimeout'
motorway_file_name_2 = '220kload-1-fault-exp-timeout'
motorway_file_name_3 = '220kload-1fault-5stimeout'

output_file_name = 'blips_zoom.pdf'

async_start = 3.8#7.6
async_end = 7.5 #9.53
hangover_end = 12.3 #12.5

hs_start_cutoff = 19
hs_end_cutoff = 25

motor_start_cutoff = 17
motor_end_cutoff = 24
motor_offset = -14.2

def plot_blip(motorway_file_name_1, hs_file_name_1, async_start, async_end, hangover_end, hs_start_cutoff, hs_end_cutoff, motor_start_cutoff, motor_end_cutoff, motor_offset, last=False):
    
    adjust_file(motorway_file_name_1, motor_offset)
   
    df_hs = pd.read_csv(hs_file_name_1 + '.txt', sep=',', header=None)
    df_hs.columns = ["start", "end", "latency"]
    df_hs['start'] = pd.to_timedelta(df_hs['start'], unit='s')
    #df['end'] = pd.to_timedelta(df['end'], unit='s')
    plot_df_hs = df_hs.groupby(pd.Grouper(key='start', freq='1s')).mean()
    #print(plot_df.index.get_level_values('start'))
    x = plot_df_hs.index.get_level_values('start') / np.timedelta64(1, 's')
    x = x.to_list()[hs_start_cutoff:-hs_end_cutoff]
    x=[a- 4 for a in x]
    y = plot_df_hs.get('latency').to_list()[hs_start_cutoff:-hs_end_cutoff]

    if last:
        plt.scatter(np.array(x), np.array(y), s=10, color='red', label='Vanilla HS', marker='v')
    else:
        plt.scatter(np.array(x), np.array(y), s=10, color='red')
    
    plt.plot(x, y, '.r-')#, ls='dashed')

   

    if last:
        plt.vlines(x=[async_start,async_end], ymin=0, ymax=6, colors='orange', ls='--', lw=2.5, label='Blip Duration')
        plt.vlines(x=[hangover_end], ymin=0, ymax=6, colors='blue', ls='dotted', lw=2.5, label='Hangover Duration')
    else:
        plt.vlines(x=[async_start,async_end], ymin=0, ymax=6, colors='orange', ls='--', lw=2.5)
        plt.vlines(x=[hangover_end], ymin=0, ymax=6, colors='blue', ls='dotted', lw=2.5)

plt.figure(figsize=(6.4,3.9))

plot_blip(motorway_file_name_2, hs_file_name_2, async_start, async_end, hangover_end, hs_start_cutoff, hs_end_cutoff, motor_start_cutoff, motor_end_cutoff, motor_offset, True)

plt.xlabel('Request start time (s)', fontsize=14)
plt.ylabel('Latency (s)', fontsize=14)
plt.xticks(np.arange(0, 17, 2), fontsize=14)
#plt.annotate('Exp.', xy =(async_start, 1), 
#                xytext =(async_start-3, 2), arrowprops = dict(facecolor ='black', 
#                                  shrink = 0.1),)
#plt.annotate('1s.', xy =(async_start_2, 1), 
#                xytext =(async_start_2-3, 2), arrowprops = dict(facecolor ='black', 
#                                  shrink = 0.1),)
#plt.annotate('5s.', xy =(async_start_3, 1), 
#                xytext =(async_start_3-3, 2), arrowprops = dict(facecolor ='black', 
#                                  shrink = 0.1),)


plt.yticks(fontsize=14)
plt.legend()
plt.tight_layout()
#plt.xlim(3, 22)
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
