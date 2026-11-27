#version 330

// ⑩ v1.5.1 近星星点 fragment：柔核 + 微辉 + 亮度驱动的十字衍射尖峰（加色混合）。

in vec2 texCoord0;
in vec4 vColor;

out vec4 fragColor;

void main() {
	vec2 p = texCoord0*2.0 - 1.0;
	float r2 = dot(p, p);
	if(r2 > 1.0) discard;
	float core = exp(-r2*7.0);
	float glow = 0.22*exp(-r2*1.8);
	float spike = (exp(-abs(p.x)*22.0)+exp(-abs(p.y)*22.0))*exp(-r2*2.0)*0.7*vColor.a;
	vec3 c = vColor.rgb*(min(core+glow, 1.0) + spike);
	fragColor = vec4(c, 1.0);
}
