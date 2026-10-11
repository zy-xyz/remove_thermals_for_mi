#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <stdlib.h>
#include <math.h>
#include <sys/stat.h>  // 新增：用于 chmod

//=================== 模糊PID控制器结构体 ===================
typedef struct {
    double kp_base, ki_base, kd_base;
    double kp_delta, ki_delta, kd_delta;
    double setpoint;            // 目标温度
    double intervention_temp;   // 介入温度 = setpoint - 2
    double overtemp_temp;       // 超温阈值 = setpoint + 0.3
    double hardware_limit_temp; // 硬件保护温度
    
    double integral;
    double prev_error;
    double output_min;
    double output_max;
    double e_range;
    double ec_range;
    double current_actual;
} FuzzyPIDController;

typedef enum {NB, NM, NS, ZO, PS, PM, PB} FuzzyValue;

//=================== 隶属函数计算 ===================
double triangle_membership(double x, double a, double b, double c) {
    if (x <= a || x >= c) return 0.0;
    if (x <= b) return (x - a) / (b - a);
    return (c - x) / (c - b);
}

void get_membership(double x, double memberships[7]) {
    double norm_x = x;
    memberships[NB] = triangle_membership(norm_x, -6, -6, -4);
    memberships[NM] = triangle_membership(norm_x, -6, -4, -2);
    memberships[NS] = triangle_membership(norm_x, -4, -2,  0);
    memberships[ZO] = triangle_membership(norm_x, -2,  0,  2);
    memberships[PS] = triangle_membership(norm_x,  0,  2,  4);
    memberships[PM] = triangle_membership(norm_x,  2,  4,  6);
    memberships[PB] = triangle_membership(norm_x,  4,  6,  6);
}

//=================== 模糊规则库 ===================
typedef struct { int kp, ki, kd; } Rule;

Rule compact_rules[4][4] = {
    // ec: 降温快 降温 稳定 升温
    /* e=过热 */ {{PB,NB,PB}, {PB,NM,PB}, {PB,NS,PB}, {PM,ZO,PM}},
    /* e=安全 */ {{PS,ZO,ZO}, {PS,ZO,NS}, {ZO,ZO,ZO}, {NS,PS,NM}},
    /* e=略低 */ {{ZO,PS,PS}, {ZO,PM,PS}, {ZO,PM,ZO}, {ZO,PB,ZO}},
    /* e=过低 */ {{NB,PB,NB}, {NB,PB,NM}, {NM,PB,NM}, {NM,PB,NM}}
};

int get_rule_index(FuzzyValue val) {
    if (val <= NS) return 0;
    if (val == ZO) return 1;
    if (val <= PS) return 2;
    return 3;
}

Rule get_compact_rule(FuzzyValue e, FuzzyValue ec) {
    return compact_rules[get_rule_index(e)][get_rule_index(ec)];
}

//=================== 模糊推理与解模糊 ===================
void calculate_fuzzy_output(FuzzyPIDController *pid, double e, double ec) {
    double norm_e = (e / pid->e_range) * 6.0;
    double norm_ec = (ec / pid->ec_range) * 6.0;
    
    double e_mem[7], ec_mem[7];
    get_membership(norm_e, e_mem);
    get_membership(norm_ec, ec_mem);
    
    pid->kp_delta = 0.0;
    pid->ki_delta = 0.0;
    pid->kd_delta = 0.0;
    double total_weight = 0.0;
    
    for (int i = 0; i < 7; i++) {
        for (int j = 0; j < 7; j++) {
            double weight = e_mem[i] * ec_mem[j];
            if (weight > 1e-6) {
                Rule rule = get_compact_rule((FuzzyValue)i, (FuzzyValue)j);
                pid->kp_delta += weight * rule.kp;
                pid->ki_delta += weight * rule.ki;
                pid->kd_delta += weight * rule.kd;
                total_weight += weight;
            }
        }
    }
    
    if (total_weight > 1e-6) {
        pid->kp_delta = (pid->kp_delta / total_weight) * 150000.0;
        pid->ki_delta = (pid->ki_delta / total_weight) * 10000.0;
        pid->kd_delta = (pid->kd_delta / total_weight) * 20000.0;
    } else {
        pid->kp_delta = pid->ki_delta = pid->kd_delta = 0.0;
    }
}

//=================== 读取当前实际电流值 ===================
int read_charge_current(void) {
    FILE *f = fopen("/sys/class/power_supply/battery/constant_charge_current", "r");
    if (!f) return -1;
    char buf[32];
    int val = -1;
    if (fgets(buf, sizeof(buf), f)) val = atoi(buf);
    fclose(f);
    return val;
}

//=================== 初始化模糊PID（参数变量化） ===================
void fuzzy_pid_init(FuzzyPIDController *pid) {
    // 基础参数
    pid->kp_base = 200000.0;
    pid->ki_base = 25000.0;
    pid->kd_base = 60000.0;
    
    // 温度参数（基于setpoint计算）
    pid->setpoint = 47.3;                   // 只需修改这里
    pid->intervention_temp = pid->setpoint - 2.0;   // 44.7℃
    pid->overtemp_temp = pid->setpoint + 0.3;       // 47.0℃
    pid->hardware_limit_temp = 48.0;               // 硬件保护温度
    
    // 输出限幅
    pid->output_min = 0.0;
    pid->output_max = 24000000.0;  // 24A（微安）
    
    // PID状态
    pid->integral = 0.0;
    pid->prev_error = 0.0;
    pid->current_actual = pid->output_max;
    
    // 模糊参数
    pid->e_range = 0.3;    // 误差范围±0.3℃
    pid->ec_range = 0.3;   // 误差变化率±0.5℃/s
}

//=================== 增量式模糊PID计算 ===================
double fuzzy_pid_calculate(FuzzyPIDController *pid, double current_temp, double dt) {
    // 读取当前实际电流值
    int actual = read_charge_current();
    if (actual >= 0) pid->current_actual = actual;
    
    // 计算温度误差
    double error = current_temp - pid->setpoint;
    double error_delta = (error - pid->prev_error) / dt;
    
    // 超温快速降温保护（使用变量参数）
    double damping_factor = 1.0;
    if (current_temp >= pid->overtemp_temp && error_delta > 0.1) {
        damping_factor = 0.2;  // 超温时增强阻尼
    }
    
    // 模糊推理动态调整PID参数
    calculate_fuzzy_output(pid, error, error_delta);
    
    double kp = pid->kp_base + pid->kp_delta;
    double ki = pid->ki_base + pid->ki_delta;
    double kd = pid->kd_base + pid->kd_delta;
    
    kp = fmax(180000.0, fmin(350000.0, kp));
    ki = fmax(0.0, fmin(8000.0, ki));
    kd = fmax(20000.0, fmin(40000.0, kd));
    
    // 积分分离：接近目标时积分，超温区停止积分
    if (error < (pid->setpoint - pid->intervention_temp)) {
        pid->integral += error * dt * (1.0 - damping_factor * 0.5);
        if (pid->integral > 40.0) pid->integral = 40.0;
        if (pid->integral < -40.0) pid->integral = -40.0;
    }
    
    // 增量式计算
    static double last_error = 0;
    double delta_error = error - last_error;
    
    double adjustment = 
        kp * delta_error +
        ki * error * dt +
        kd * (error - 2*pid->prev_error + last_error);
    
    // 超温直接惩罚
    if (error > 0) {
        adjustment -= error * 20000000.0;  // 每超0.1度减2A
    }
    
    // 在当前实际值基础上调整
    double new_output = pid->current_actual + adjustment;
    
    // 软限制：使用变量参数
    if (current_temp > pid->overtemp_temp) {
        double max_allowed = pid->output_max * (pid->hardware_limit_temp - current_temp) / 
                             (pid->hardware_limit_temp - pid->overtemp_temp);
        if (max_allowed < 0) max_allowed = 0;
        if (new_output > max_allowed) new_output = max_allowed;
    }
    
    // 标准限幅
    if (new_output < pid->output_min) new_output = pid->output_min;
    if (new_output > pid->output_max) new_output = pid->output_max;
    
    // 更新历史值
    last_error = pid->prev_error;
    pid->prev_error = error;
    
    // 打印信息
    printf("T:%4.1f℃->%4.1fA (E:%+4.2f Δ:%+5.0fmA Act:%4.1fA)\n",
           current_temp, new_output / 1000000.0, error,
           adjustment / 1000.0, pid->current_actual / 1000000.0);
    
    return new_output;
}

//=================== 文件操作函数 ===================
int read_int_from_file(const char *path) {
    FILE *f = fopen(path, "r");
    if (!f) return -1;
    char buf[32];
    int val = -1;
    if (fgets(buf, sizeof(buf), f)) val = atoi(buf);
    fclose(f);
    return val;
}

// 新增：设置文件权限函数
int set_file_permission(const char *path, mode_t mode) {
    if (chmod(path, mode) != 0) {
        perror("chmod失败");
        return -1;
    }
    //printf("成功设置 %s 权限为 %03o\n", path, mode);
    return 0;
}

// 新增：通用属性设置函数（图片中的方式）
void set_prop(const char *file_path, int target_value) {
    FILE *file = fopen(file_path, "w");
    if (file != NULL) {
        fprintf(file, "%d", target_value);
        fclose(file);
    } else {
        perror("fopen失败");
    }
}

// 修改：使用set_prop设置充电电流
void set_charge_current(int current_ua) {
    if (current_ua < 0) current_ua = 0;
    if (current_ua > 24000000) current_ua = 24000000;
    
    // 先尝试修改权限（对sysfs无效，但用户要求）
    set_file_permission("/sys/class/power_supply/battery/constant_charge_current", 0777);
    
    set_prop("/sys/class/power_supply/battery/constant_charge_current", current_ua);
}

//=================== 主函数 ===================
int main() {
    char buffer[128];
    static char prev_status[128] = "";
    FuzzyPIDController pid;
    fuzzy_pid_init(&pid);
    
    printf("=== 增量式模糊PID温控充电 ===\n");
    printf("目标:%.1f℃ | 介入:%.1f℃ | 超温:%.1f℃ | 保护:%.1f℃\n",
           pid.setpoint, pid.intervention_temp, pid.overtemp_temp, pid.hardware_limit_temp);
    
    // 初始化读取一次实际电流
    int initial = read_charge_current();
    if (initial > 0) pid.current_actual = initial;
    
    while (1) {
        FILE *fp = fopen("/sys/class/power_supply/battery/status", "r");
        if (fp && fgets(buffer, sizeof(buffer), fp)) {
            buffer[strcspn(buffer, "\n")] = 0;
            if (strcmp(buffer, prev_status) != 0) {
                if (strstr(buffer, "Discharging")) {
                    set_charge_current(0);
                    pid.integral = 0;
                }
                strncpy(prev_status, buffer, sizeof(prev_status) - 1);
            }
            fclose(fp);
        }
        
        if (!strstr(prev_status, "Discharging")) {
            int temp_raw = read_int_from_file("/sys/class/power_supply/battery/temp");
            if (temp_raw > 0) {
                double temp = temp_raw / 10.0;
                double current = pid.output_max;
                
                // 判断语句：当温度低于干预温度时，电流取最大值
                if (temp < pid.intervention_temp) {
                    set_charge_current((int)pid.output_max);
                    printf("温度 %.1f℃ < %.1f℃，使用最大电流 %.1fA\n", 
                           temp, pid.intervention_temp, pid.output_max / 1000000.0);
                }
                // 使用变量参数：目标温度前2度开始PID控制
                else if (temp >= pid.intervention_temp) {
                    current = fuzzy_pid_calculate(&pid, temp, 1.0);
                    set_charge_current((int)current);
                }
            }
        }
        
        sleep(1);
    }
    
    return 0;
}
