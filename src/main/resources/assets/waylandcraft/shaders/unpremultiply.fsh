#version 330

uniform sampler2D Sampler0;

in vec2 texCoord;

out vec4 fragColor;

void main() {
	vec4 color = texture(Sampler0, texCoord);
	// alpha==0 时除零产生 NaN/Inf（会污染下游 blend/composite）；下限 1e-6
	// 对任何 alpha>0 的纹理（8bit 下最小 alpha=1/255≈0.0039）数值完全不变，
	// alpha==0 的 premultiplied 像素 rgb 恒为 0，0/1e-6 仍是 0。
	color.rgb /= max(color.a, 1e-6);
	fragColor = color;
}
