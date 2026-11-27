#version 330
#moj_import <minecraft:projection.glsl>
#moj_import <minecraft:dynamictransforms.glsl>

// ⑩ v1.5.0 沉浸模式星空穹顶：相机为中心的立方壳（Position = 相机相对坐标，
// 相机恒在原点 → Position 即方向向量×半径）。UV0.x 携带时间（秒）驱动闪烁/
// 吸积盘旋转，避免新增 uniform 依赖（submit 路径没有 setUniform 时机）。

in vec3 Position;
in vec2 UV0;

out vec2 texCoord0;
out vec3 vDir;

void main() {
	gl_Position = ProjMat * ModelViewMat * vec4(Position, 1.0);
	texCoord0 = UV0;
	vDir = Position;
}
