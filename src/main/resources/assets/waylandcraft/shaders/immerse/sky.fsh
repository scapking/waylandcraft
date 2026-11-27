#version 330

// ⑩ v1.5.1 沉浸模式星空穹顶 fragment（HTML 预览 v3.2 用户验收版的 1:1 移植）。
// 全程序化（无纹理无采样），单 pass：
// - 动态银河：绕轴自转（Rodrigues，周期 ~9 分钟）+ 尘埃独立漂移 + 丝状暗脉
// - 银河带内远景微星（3D 网格，无缝）
// - domain-warp 星云（靛青↔洋红絮流）
// - 月亮（月海/月牙相位/双层晕）、金/蓝白双远景恒星（星核+分层辉光）
// - 黑洞：多普勒增亮（逼近侧蓝白/远离侧深橙红）+ 双层光子环 + 影缘聚光弧
// - 全部噪声作用于方向向量本身（3D 无缝——无八面体投影棱边割裂）
// - ACES 色调映射 + 曝光 + 伽马；角度暗角（vEdge 由 vsh 传入）

in vec2 texCoord0;
in vec3 vDir;
in float vEdge;

out vec4 fragColor;

float hash21(vec2 p){ return fract(sin(dot(p, vec2(127.1,311.7)))*43758.5453123); }
float hash31(vec3 p){ return fract(sin(dot(p, vec3(127.1,311.7,74.7)))*43758.5453123); }
float noise2(vec2 p){
	vec2 i=floor(p), f=fract(p); f=f*f*(3.0-2.0*f);
	return mix(mix(hash21(i),hash21(i+vec2(1,0)),f.x),
	           mix(hash21(i+vec2(0,1)),hash21(i+vec2(1,1)),f.x), f.y);
}
float noise3(vec3 p){
	vec3 i=floor(p), f=fract(p); f=f*f*(3.0-2.0*f);
	float n000=hash31(i), n100=hash31(i+vec3(1,0,0)), n010=hash31(i+vec3(0,1,0)), n110=hash31(i+vec3(1,1,0));
	float n001=hash31(i+vec3(0,0,1)), n101=hash31(i+vec3(1,0,1)), n011=hash31(i+vec3(0,1,1)), n111=hash31(i+vec3(1,1,1));
	return mix(mix(mix(n000,n100,f.x), mix(n010,n110,f.x), f.y),
	           mix(mix(n001,n101,f.x), mix(n011,n111,f.x), f.y), f.z);
}
float fbm3(vec3 p){
	float v=0.0, a=0.5;
	for(int i=0;i<4;i++){ v+=a*noise3(p); p=p*2.07+vec3(11.7,7.3,5.1); a*=0.5; }
	return v;
}

// 远景微星（3D 方向网格：球面无缝）
float starField3(vec3 d, float density, float time, float twinkle){
	vec3 g = d*density;
	vec3 cell = floor(g);
	vec3 f = g - cell;
	float rnd = hash31(cell);
	if(rnd < 0.965) return 0.0;
	vec3 pos = vec3(hash31(cell+13.1), hash31(cell+5.7), hash31(cell+9.3));
	float dist = length(f-pos);
	float tw = 0.75+0.25*sin(time*(1.5+2.0*hash31(cell+2.9))+rnd*20.0);
	return exp(-dist*dist*45.0)*mix(1.0,tw,twinkle)*0.8;
}

void main() {
	vec3 d = normalize(vDir);
	float time = texCoord0.x;

	float horizon = 1.0 - abs(d.y);
	vec3 col = mix(vec3(0.003,0.004,0.010), vec3(0.016,0.020,0.042), horizon);
	col += vec3(0.010,0.004,0.020)*pow(max(-d.y,0.0),1.5);

	// ===== 动态银河：绕轴自转 + 尘埃漂移 =====
	vec3 galN = normalize(vec3(0.42, 0.78, -0.46));
	float galRot = time * 0.012;
	float grc = cos(galRot), grs = sin(galRot);
	vec3 dGal = d*grc + cross(galN, d)*grs + galN*dot(galN, d)*(1.0-grc);
	float bandDist = abs(dot(d, galN));
	float band = exp(-bandDist*bandDist*22.0);
	float dust = fbm3(dGal*6.5 + vec3(3.1,7.7,1.9) + vec3(0.0, 0.0, time*0.006));
	float dustMask = smoothstep(0.32, 0.62, dust);
	float galactic = band * (0.35 + 0.65*dustMask);
	vec3 galCol = mix(vec3(0.05,0.06,0.11), vec3(0.30,0.24,0.18), dust*0.7);
	col += galCol * galactic * 1.15;
	float vein = smoothstep(0.45,0.75,fbm3(dGal*13.0 - vec3(1.0,time*0.012,0.5)));
	col *= 1.0 - band*vein*0.55;

	// 银河带内远景微星
	float micro = starField3(dGal, 60.0, time, 0.10);
	col += vec3(0.75,0.80,0.95) * micro * (0.30 + band*2.2);
	float micro2 = starField3(dGal + 0.13, 130.0, time, 0.35);
	col += vec3(0.80,0.85,1.0) * micro2 * (0.18 + band*1.1);

	// ===== 星云：3D domain-warp =====
	vec3 wq = dGal*2.4 + vec3(noise3(dGal*2.0 + time*0.02), noise3(dGal*2.0 + 7.3 - time*0.015), noise3(dGal*2.0 + 3.1 + time*0.011));
	float nebA = fbm3(wq*3.2);
	float nebB = fbm3(d*2.0 + 4.2);
	float neb = pow(max(nebA-0.42,0.0),1.35)*1.7;
	vec3 nebCol = mix(vec3(0.07,0.14,0.40), vec3(0.30,0.10,0.28), smoothstep(0.35,0.75,nebB));
	col += nebCol * neb;

	// ===== 月亮 =====
	vec3 moonDir = normalize(vec3(0.40,0.55,-0.72));
	float md = dot(d, moonDir);
	if(md > 0.99950){
		vec3 mx = normalize(cross(moonDir, vec3(0.0,1.0,0.0)));
		vec3 my = cross(mx, moonDir);
		vec2 lp = vec2(dot(d,mx), dot(d,my))*90.0;
		float phase = clamp(0.5+0.55*lp.x, 0.0, 1.0);
		float mare = 0.80 + 0.20*noise2(lp*4.5+1.3);
		float fine = 0.92 + 0.08*noise2(lp*14.0);
		float lit = mix(0.10, 1.0, smoothstep(0.0,1.0,phase));
		col = mix(col, vec3(0.95,0.94,0.90)*lit*mare*fine, smoothstep(0.99950,0.99980,md));
	}
	col += vec3(0.50,0.56,0.72)*pow(max(md-0.993,0.0),3.0)*0.30;
	col += vec3(0.62,0.66,0.80)*pow(max(md-0.9975,0.0),2.0)*0.20;

	// ===== 远景恒星 =====
	vec3 goldDir = normalize(vec3(-0.66,0.18,0.72));
	vec3 blueDir = normalize(vec3(0.15,0.78,0.60));
	float gd = max(dot(d,goldDir),0.0), bd2 = max(dot(d,blueDir),0.0);
	col += vec3(1.00,0.86,0.58)*pow(gd,6000.0)*(0.85+0.15*sin(time*0.7))*1.2;
	col += vec3(0.36,0.26,0.12)*pow(gd,180.0)*0.7;
	col += vec3(0.70,0.80,1.00)*pow(bd2,9000.0)*(0.85+0.15*sin(time*0.9+2.0))*1.2;
	col += vec3(0.10,0.18,0.36)*pow(bd2,240.0)*0.7;

	// ===== 黑洞：多普勒增亮 =====
	vec3 bhDir = normalize(vec3(-0.62,0.28,0.73));
	float ang = acos(clamp(dot(d,bhDir),-1.0,1.0));
	vec3 bx = normalize(cross(bhDir, vec3(0.0,1.0,0.0)));
	vec3 by = cross(bx, bhDir);
	float dopG = pow(0.5+0.5*sin(atan(dot(d,bx), dot(d,by))+2.2), 2.0);
	float asym = 1.0 + 0.35*sin(atan(dot(d,bx), dot(d,by)) + 0.9);
	float band2 = smoothstep(0.040,0.068,ang)*(1.0-smoothstep(0.125,0.175,ang/asym));
	float planeW = 1.0-smoothstep(0.20,0.80,abs(dot(d,normalize(cross(bx,by)))));
	float disk = band2*mix(0.12,1.0,planeW);
	float core = 1.0-smoothstep(0.026,0.038,ang);
	float ring1 = smoothstep(0.030,0.0345,ang)*(1.0-smoothstep(0.0345,0.040,ang));
	float ring2 = smoothstep(0.042,0.048,ang)*(1.0-smoothstep(0.048,0.056,ang));
	if(disk > 0.001){
		vec2 dp = vec2(dot(d,bx), dot(d,by));
		float angP = atan(dp.y, dp.x);
		float swirl = fbm3(vec3(cos(angP), sin(angP), ang*9.0)*3.0 + vec3(time*1.1, -time*0.7, 0.0))*0.65
		            + noise3(vec3(cos(angP)*2.0, sin(angP)*2.0, ang*24.0) + vec3(-time*1.9, 0.0, 0.0))*0.35;
		float heat = smoothstep(0.16,0.055,ang);
		float dop = pow(0.5 + 0.5*sin(angP + 2.2), 2.2);
		vec3 hot = mix(vec3(1.00,0.94,0.80), vec3(0.85,0.92,1.00), dop*0.6);
		vec3 cool = mix(vec3(0.95,0.42,0.12), vec3(0.55,0.10,0.05), 1.0-dop);
		vec3 diskCol = mix(cool, hot, dop*heat);
		col = mix(col, diskCol*((0.30+1.1*swirl)*(0.55+1.35*dop)), disk*0.92);
	}
	col = mix(col, vec3(0.0), core);
	col += vec3(1.0,0.94,0.82)*ring1*(0.85+0.35*dopG);
	col += vec3(0.85,0.72,0.55)*ring2*0.35;
	float arc = smoothstep(0.038,0.042,ang)*(1.0-smoothstep(0.042,0.052,ang))*0.25;
	col += vec3(0.9,0.8,0.6)*arc;

	// 角度暗角（vEdge 由 vsh 传入，屏幕无缝）
	col *= max(1.0 - 0.22*vEdge*vEdge, 0.0);

	// ACES 色调映射 + 曝光 + 伽马
	col *= 1.25;
	col = (col*(2.51*col+0.03))/(col*(2.43*col+0.59)+0.14);
	col = pow(max(col,0.0), vec3(1.0/2.2));
	fragColor = vec4(col, 1.0);
}
