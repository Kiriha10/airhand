// Desktop pixels contain sRGB values in an UNORM texture. Keep their transfer
// function, and convert the matrix/range to BT.709 limited-range NV12.
Texture2D<float4> desktop : register(t0);

float4 vertex(uint id : SV_VertexID) : SV_Position {
    float2 uv = float2((id << 1) & 2, id & 2);
    return float4(uv * float2(2, -2) + float2(-1, 1), 0, 1);
}

float luma(float4 position : SV_Position) : SV_Target {
    float3 rgb = desktop.Load(int3(position.xy, 0)).rgb;
    return (16.0 + 219.0 * dot(rgb, float3(0.2126, 0.7152, 0.0722))) / 255.0;
}

float2 chroma(float4 position : SV_Position) : SV_Target {
    int2 origin = int2(position.xy) * 2;
    // Average all four pixels. A driver's left-sited video processor may only
    // sample the first column, amplifying ClearType's coloured text edges.
    float3 rgb = (
        desktop.Load(int3(origin, 0)).rgb +
        desktop.Load(int3(origin + int2(1, 0), 0)).rgb +
        desktop.Load(int3(origin + int2(0, 1), 0)).rgb +
        desktop.Load(int3(origin + int2(1, 1), 0)).rgb
    ) * 0.25;
    float y = dot(rgb, float3(0.2126, 0.7152, 0.0722));
    return (128.0 + 224.0 * float2((rgb.b - y) / 1.8556, (rgb.r - y) / 1.5748)) / 255.0;
}
