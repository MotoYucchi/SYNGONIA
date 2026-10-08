#include <windows.h>
#include <d3d11.h>
#include <d3dcompiler.h>
#include <stdio.h>
#include <string.h>
#include <math.h>
#include "SpoutDX.h"

// SYNGONIA Native Spout2 DirectX 11 GPU Shader Engine
// Ultra-low latency GPU-native rendering pipeline for OBS / Resolume integration.

#pragma comment(lib, "d3dcompiler.lib")

// Force NVIDIA Optimus and AMD PowerXpress to select High Performance Discrete GPU (RTX 4060)
extern "C" {
    __declspec(dllexport) DWORD NvOptimusEnablement = 0x00000001;
    __declspec(dllexport) int AmdPowerXpressRequestHighPerformance = 1;
}

// Render Parameters struct matching HLSL cbuffer (16-byte aligned, 112 bytes total)
#pragma pack(push, 16)
struct RenderParams {
    float time;
    float bass;
    float mid;
    float high;

    float beat;
    int   effect_mode;
    int   blackout;
    float pad0;

    float layer_opacity[4]; // CH1..CH4
    float layer_color1[4];  // RGBA
    float layer_color2[4];  // RGBA
    float layer_color3[4];  // RGBA
    float layer_color4[4];  // RGBA
};
#pragma pack(pop)

static spoutDX* g_spout = nullptr;
static char g_senderName[256] = "SYNGONIA";

// Direct3D 11 Render Pipeline Resources
static ID3D11Texture2D*          g_renderTargetTex = nullptr;
static ID3D11RenderTargetView*   g_renderTargetView = nullptr;
static ID3D11VertexShader*       g_vertexShader = nullptr;
static ID3D11PixelShader*        g_pixelShader = nullptr;
static ID3D11Buffer*             g_constantBuffer = nullptr;
static ID3D11RasterizerState*    g_rasterizerState = nullptr;
static D3D11_VIEWPORT            g_viewport = { 0.0f, 0.0f, 1920.0f, 1080.0f, 0.0f, 1.0f };
static unsigned int              g_renderWidth = 1920;
static unsigned int              g_renderHeight = 1080;

// Embedded High-Performance HLSL Shader (SM 5.0)
static const char* g_hlslShaderSource = R"(
cbuffer SyngoniaParams : register(b0) {
    float u_time;
    float u_bass;
    float u_mid;
    float u_high;
    float u_beat;
    int   u_effect_mode;
    int   u_blackout;
    float u_pad0;
    float4 u_layer_opacity; // x: CH1, y: CH2, z: CH3, w: CH4 (0.0 .. 1.0)
    float4 u_layer_color1;  // RGBA
    float4 u_layer_color2;  // RGBA
    float4 u_layer_color3;  // RGBA
    float4 u_layer_color4;  // RGBA
};

struct VSOutput {
    float4 pos : SV_Position;
    float2 uv  : TEXCOORD0;
};

// Fullscreen Triangle Vertex Shader (0 vertex buffers required)
VSOutput VSMain(uint id : SV_VertexID) {
    VSOutput output;
    output.uv = float2((id << 1) & 2, id & 2);
    output.pos = float4(output.uv * float2(2.0f, -2.0f) + float2(-1.0f, 1.0f), 0.0f, 1.0f);
    return output;
}

// --- Mode 0: Broadcast SMPTE Test Pattern + Animated Cyan Bar & Beat Box ---
float4 RenderTestPattern(float2 uv) {
    float3 smpte[7] = {
        float3(0.75, 0.75, 0.75), // 75% White
        float3(0.75, 0.75, 0.0),  // Yellow
        float3(0.0,  0.75, 0.75), // Cyan
        float3(0.0,  0.75, 0.0),  // Green
        float3(0.75, 0.0,  0.75), // Magenta
        float3(0.75, 0.0,  0.0),  // Red
        float3(0.0,  0.0,  0.75)  // Blue
    };

    float3 color = float3(0.0, 0.0, 0.0);
    if (uv.y < 0.70) {
        int idx = clamp((int)(uv.x * 7.0), 0, 6);
        color = smpte[idx];
    } else if (uv.y < 0.85) {
        color = float3(uv.x, uv.x, uv.x);
    } else {
        color = float3(0.06, 0.08, 0.11);
        float pingpong = abs(frac(u_time * 0.3) * 2.0 - 1.0);
        float blockStart = pingpong * 0.88;
        if (uv.x >= blockStart && uv.x <= blockStart + 0.12) {
            color = float3(0.0, 1.0, 1.0);
        }
        if (uv.x > 0.92 && uv.y > 0.92) {
            color = (frac(u_time) < 0.25 || u_beat > 0.5) ? float3(1.0, 0.1, 0.2) : float3(0.2, 0.02, 0.04);
        }
    }

    float2 diff = abs(uv - 0.5);
    if ((diff.x < 0.0006 || diff.y < 0.0012) && frac(uv.x * 120.0) < 0.5) {
        color = float3(1.0, 1.0, 1.0);
    }

    return float4(color, 1.0);
}

// --- Mode 1: 3D Cyber Tunnel (Infinite Hyperspace Tunnel) ---
float4 RenderCyberTunnel(float2 uv, float4 colTint) {
    float2 p = uv - 0.5;
    p.x *= 1.7777778;
    float r = length(p);
    float a = atan2(p.y, p.x);

    float z = 0.25 / (r + 0.001) + u_time * (1.2 + u_bass * 1.8);
    float angleCoord = (a / 3.14159265 + 1.0) * 4.0;

    float grid = abs(sin(z * 6.28318)) * abs(sin(angleCoord * 3.14159265));
    grid = pow(saturate(grid), 8.0);

    float rings = abs(sin(z * 12.56637));
    rings = smoothstep(0.82 - u_bass * 0.18, 1.0, rings);

    float3 baseCol = (colTint.r + colTint.g + colTint.b > 0.05) ? colTint.rgb : float3(0.0, 0.9, 1.0);
    float3 glowCol = baseCol + u_bass * float3(0.6, 0.2, 0.1);
    float3 finalCol = (grid * 2.2 + rings * 2.8) * glowCol * r + (baseCol * rings * 0.4);

    finalCol *= smoothstep(0.01, 0.35, r);
    return float4(finalCol, 1.0);
}

// --- Mode 2: Audio-Reactive Raymarching Sphere / Fractal Core ---
float4 RenderAudioRaymarch(float2 uv, float4 colTint) {
    float2 p = (uv - 0.5) * float2(1.7777778, 1.0);
    float3 ro = float3(0.0, 0.0, -2.8);
    float3 rd = normalize(float3(p, 1.4));

    float ca = u_time * 0.5 + u_mid * 0.6;
    float s = sin(ca), c = cos(ca);
    float2x2 rot = float2x2(c, -s, s, c);
    ro.xz = mul(rot, ro.xz);
    rd.xz = mul(rot, rd.xz);

    float t = 0.0;
    float glow = 0.0;
    for (int i = 0; i < 20; i++) {
        float3 pos = ro + rd * t;
        float baseDist = length(pos) - (1.0 + u_bass * 0.5);
        float gyroid = dot(sin(pos * (3.5 + u_high * 4.0)), cos(pos.zxy * (3.5 + u_high * 4.0))) * 0.14;
        float d = baseDist + gyroid;
        glow += 0.025 / (abs(d) + 0.04 + (1.0 - u_bass) * 0.03);
        if (d < 0.005 || t > 7.0) break;
        t += d * 0.75;
    }

    float3 baseCol = (colTint.r + colTint.g + colTint.b > 0.05) ? colTint.rgb : float3(1.0, 0.3, 0.8);
    float3 col = glow * (baseCol + float3(0.1, 0.15, 0.3) + u_bass * float3(0.5, 0.1, 0.0));
    return float4(saturate(col), 1.0);
}

// --- Mode 3: Neon Wave Grid (Synthwave / Cyberpunk Horizon) ---
float4 RenderNeonGrid(float2 uv, float4 colTint) {
    float2 p = (uv - 0.5) * float2(1.7777778, 1.0);
    float3 col = float3(0.02, 0.01, 0.04);

    if (p.y < 0.0) {
        float py = -p.y;
        float depth = 0.38 / (py + 0.001);
        float px = p.x * depth;

        float2 grid = abs(frac(float2(px, depth + u_time * (2.2 + u_bass * 3.5))) - 0.5);
        float lineVal = min(grid.x, grid.y);
        float intensity = smoothstep(0.08, 0.01, lineVal) * exp(-py * 3.5);

        float3 baseCol = (colTint.r + colTint.g + colTint.b > 0.05) ? colTint.rgb : float3(0.2, 0.9, 0.4);
        col += intensity * baseCol * (1.6 + u_bass * 1.5);
    } else {
        float wave = sin(p.x * 8.0 + u_time * 3.0) * (0.05 + u_mid * 0.14);
        float dist = abs(p.y - wave - 0.06);
        float beam = smoothstep(0.04 + u_high * 0.04, 0.0, dist);
        col += beam * float3(1.0, 0.2, 0.6) * 1.6;
    }

    return float4(saturate(col), 1.0);
}

// --- Mode 4: Acid Glitch Matrix (サビ・ドロップ用瞬間キメ) ---
float4 RenderAcidGlitch(float2 uv, float4 colTint) {
    float2 g_uv = uv;
    if (u_high > 0.35 || u_beat > 0.5) {
        float slice = floor(uv.y * 32.0);
        float rnd = sin(slice * 133.456 + u_time * 12.0);
        if (frac(rnd * 10.0) > 0.5) {
            g_uv.x += (rnd - 0.5) * 0.08 * (u_high + u_beat);
        }
    }

    float shift = 0.008 + u_bass * 0.025;
    float r = sin((g_uv.x + shift) * 36.0 + u_time * 6.0) * cos(g_uv.y * 18.0);
    float g = sin(g_uv.x * 36.0 + u_time * 6.0) * cos(g_uv.y * 18.0);
    float b = sin((g_uv.x - shift) * 36.0 + u_time * 6.0) * cos(g_uv.y * 18.0);

    float3 col = float3(r, g, b) * 0.5 + 0.5;
    float3 baseCol = (colTint.r + colTint.g + colTint.b > 0.05) ? colTint.rgb : float3(1.0, 0.9, 0.1);
    col *= baseCol * 1.6;

    col *= 0.85 + 0.15 * sin(uv.y * 1080.0 * 1.5);
    return float4(saturate(col), 1.0);
}

// --- Main Pixel Shader ---
float4 PSMain(VSOutput input) : SV_Target {
    if (u_blackout == 1) {
        return float4(0.0, 0.0, 0.0, 1.0);
    }

    float2 uv = input.uv;

    // Single effect mode bypass
    if (u_effect_mode == 0) return RenderTestPattern(uv);
    if (u_effect_mode == 1) return RenderCyberTunnel(uv, u_layer_color1);
    if (u_effect_mode == 2) return RenderAudioRaymarch(uv, u_layer_color2);
    if (u_effect_mode == 3) return RenderNeonGrid(uv, u_layer_color3);
    if (u_effect_mode == 4) return RenderAcidGlitch(uv, u_layer_color4);

    // Multi-layer Resolume Style Composite (Mode 5 or higher)
    float4 ch1 = RenderCyberTunnel(uv, u_layer_color1)  * saturate(u_layer_opacity.x);
    float4 ch2 = RenderAudioRaymarch(uv, u_layer_color2) * saturate(u_layer_opacity.y);
    float4 ch3 = RenderNeonGrid(uv, u_layer_color3)      * saturate(u_layer_opacity.z);
    float4 ch4 = RenderAcidGlitch(uv, u_layer_color4)    * saturate(u_layer_opacity.w);

    // Screen / Add blend compositing in VRAM
    float3 comp = ch1.rgb;
    comp = 1.0 - (1.0 - comp) * (1.0 - ch2.rgb); // Screen blend
    comp += ch3.rgb;                              // Add blend
    comp = 1.0 - (1.0 - comp) * (1.0 - ch4.rgb); // Screen blend

    return float4(saturate(comp), 1.0);
}
)";

// Cleanup Direct3D 11 Render Pipeline
static void ReleaseD3D11Renderer() {
    if (g_rasterizerState) { g_rasterizerState->Release(); g_rasterizerState = nullptr; }
    if (g_constantBuffer)   { g_constantBuffer->Release();   g_constantBuffer = nullptr; }
    if (g_pixelShader)      { g_pixelShader->Release();      g_pixelShader = nullptr; }
    if (g_vertexShader)     { g_vertexShader->Release();     g_vertexShader = nullptr; }
    if (g_renderTargetView) { g_renderTargetView->Release(); g_renderTargetView = nullptr; }
    if (g_renderTargetTex)  { g_renderTargetTex->Release();  g_renderTargetTex = nullptr; }
}

// Initialize Direct3D 11 Render Pipeline on active SpoutDX Device
static bool InitD3D11Renderer(unsigned int width, unsigned int height) {
    if (!g_spout) return false;

    ID3D11Device* pDevice = g_spout->GetDX11Device();
    if (!pDevice) {
        printf("[SPOUT DX ERROR] GetDX11Device returned null!\n");
        return false;
    }

    ReleaseD3D11Renderer();

    g_renderWidth = width;
    g_renderHeight = height;

    // 1. Create Render Target Texture (BGRA for Spout / OBS)
    D3D11_TEXTURE2D_DESC texDesc = {};
    texDesc.Width = width;
    texDesc.Height = height;
    texDesc.MipLevels = 1;
    texDesc.ArraySize = 1;
    texDesc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
    texDesc.SampleDesc.Count = 1;
    texDesc.SampleDesc.Quality = 0;
    texDesc.Usage = D3D11_USAGE_DEFAULT;
    texDesc.BindFlags = D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE;
    texDesc.CPUAccessFlags = 0;
    texDesc.MiscFlags = 0;

    HRESULT hr = pDevice->CreateTexture2D(&texDesc, nullptr, &g_renderTargetTex);
    if (FAILED(hr)) {
        printf("[SPOUT DX ERROR] CreateTexture2D failed (0x%08X)\n", hr);
        return false;
    }

    // 2. Create Render Target View
    hr = pDevice->CreateRenderTargetView(g_renderTargetTex, nullptr, &g_renderTargetView);
    if (FAILED(hr)) {
        printf("[SPOUT DX ERROR] CreateRenderTargetView failed (0x%08X)\n", hr);
        ReleaseD3D11Renderer();
        return false;
    }

    // 3. Compile Shaders
    ID3DBlob* vsBlob = nullptr;
    ID3DBlob* psBlob = nullptr;
    ID3DBlob* errBlob = nullptr;

    UINT compileFlags = D3DCOMPILE_ENABLE_STRICTNESS | D3DCOMPILE_OPTIMIZATION_LEVEL3;

    hr = D3DCompile(g_hlslShaderSource, strlen(g_hlslShaderSource), "SyngoniaShader", nullptr, nullptr, "VSMain", "vs_5_0", compileFlags, 0, &vsBlob, &errBlob);
    if (FAILED(hr)) {
        if (errBlob) {
            printf("[SPOUT DX ERROR] VS Compile Error:\n%s\n", (char*)errBlob->GetBufferPointer());
            errBlob->Release();
        }
        ReleaseD3D11Renderer();
        return false;
    }

    hr = D3DCompile(g_hlslShaderSource, strlen(g_hlslShaderSource), "SyngoniaShader", nullptr, nullptr, "PSMain", "ps_5_0", compileFlags, 0, &psBlob, &errBlob);
    if (FAILED(hr)) {
        if (errBlob) {
            printf("[SPOUT DX ERROR] PS Compile Error:\n%s\n", (char*)errBlob->GetBufferPointer());
            errBlob->Release();
        }
        vsBlob->Release();
        ReleaseD3D11Renderer();
        return false;
    }

    hr = pDevice->CreateVertexShader(vsBlob->GetBufferPointer(), vsBlob->GetBufferSize(), nullptr, &g_vertexShader);
    vsBlob->Release();
    if (FAILED(hr)) {
        psBlob->Release();
        ReleaseD3D11Renderer();
        return false;
    }

    hr = pDevice->CreatePixelShader(psBlob->GetBufferPointer(), psBlob->GetBufferSize(), nullptr, &g_pixelShader);
    psBlob->Release();
    if (FAILED(hr)) {
        ReleaseD3D11Renderer();
        return false;
    }

    // 4. Create Constant Buffer
    D3D11_BUFFER_DESC cbDesc = {};
    cbDesc.ByteWidth = sizeof(RenderParams);
    cbDesc.Usage = D3D11_USAGE_DEFAULT;
    cbDesc.BindFlags = D3D11_BIND_CONSTANT_BUFFER;
    cbDesc.CPUAccessFlags = 0;

    hr = pDevice->CreateBuffer(&cbDesc, nullptr, &g_constantBuffer);
    if (FAILED(hr)) {
        printf("[SPOUT DX ERROR] CreateBuffer for constant buffer failed (0x%08X)\n", hr);
        ReleaseD3D11Renderer();
        return false;
    }

    // 5. Create Rasterizer State (No culling)
    D3D11_RASTERIZER_DESC rastDesc = {};
    rastDesc.FillMode = D3D11_FILL_SOLID;
    rastDesc.CullMode = D3D11_CULL_NONE;
    rastDesc.DepthClipEnable = FALSE;

    hr = pDevice->CreateRasterizerState(&rastDesc, &g_rasterizerState);
    if (FAILED(hr)) {
        printf("[SPOUT DX ERROR] CreateRasterizerState failed (0x%08X)\n", hr);
        ReleaseD3D11Renderer();
        return false;
    }

    // 6. Viewport setup
    g_viewport.TopLeftX = 0.0f;
    g_viewport.TopLeftY = 0.0f;
    g_viewport.Width = (float)width;
    g_viewport.Height = (float)height;
    g_viewport.MinDepth = 0.0f;
    g_viewport.MaxDepth = 1.0f;

    printf("[SPOUT DX] GPU Native Shader Pipeline initialized successfully (%ux%u)\n", width, height);
    return true;
}

extern "C" {
    __declspec(dllexport) void* Spout_Init(const char* sender_name) {
        if (!g_spout) {
            g_spout = new spoutDX();
        }

        if (sender_name && sender_name[0] != '\0') {
            strncpy_s(g_senderName, sizeof(g_senderName), sender_name, sizeof(g_senderName) - 1);
        } else {
            strcpy_s(g_senderName, sizeof(g_senderName), "SYNGONIA");
        }

        int numAdapters = g_spout->GetNumAdapters();
        printf("[SPOUT DX] System graphics adapters detected: %d\n", numAdapters);
        int preferredAdapter = 0;
        bool foundNvidia = false;

        for (int i = 0; i < numAdapters; i++) {
            char name[256] = {};
            g_spout->GetAdapterName(i, name, 256);
            printf("  -> Adapter [%d]: %s\n", i, name);

            if (!foundNvidia && (strstr(name, "NVIDIA") || strstr(name, "GeForce") || strstr(name, "RTX"))) {
                preferredAdapter = i;
                foundNvidia = true;
            }
        }

        printf("[SPOUT DX] Selecting Adapter [%d] for OBS alignment\n", preferredAdapter);
        g_spout->SetAdapter(preferredAdapter);

        if (!g_spout->OpenDirectX11()) {
            printf("[SPOUT DX WARN] OpenDirectX11 failed on adapter [%d], falling back to default...\n", preferredAdapter);
            g_spout->SetAdapter(0);
            if (!g_spout->OpenDirectX11()) {
                printf("[SPOUT DX ERROR] OpenDirectX11 completely failed!\n");
                delete g_spout;
                g_spout = nullptr;
                return nullptr;
            }
        }

        char desc[256] = {};
        g_spout->GetAdapterName(g_spout->GetAdapter(), desc, 256);
        printf("[SPOUT DX] Active Adapter index: %d (%s)\n", g_spout->GetAdapter(), desc);

        g_spout->ReleaseSender();
        g_spout->SetSenderName(g_senderName);
        g_spout->SetSenderFormat(DXGI_FORMAT_B8G8R8A8_UNORM);

        // Initialize GPU Shader Pipeline (1080p default)
        InitD3D11Renderer(1920, 1080);

        printf("[SPOUT DX] Sender '%s' ready with GPU Shader Engine\n", g_spout->GetName());
        return g_spout;
    }

    __declspec(dllexport) bool Spout_SetAdapter(void* handle, int adapterIndex) {
        if (!handle) return false;
        spoutDX* spout = static_cast<spoutDX*>(handle);

        printf("[SPOUT DX] Re-binding Direct3D 11 to Adapter [%d]...\n", adapterIndex);
        ReleaseD3D11Renderer();
        spout->ReleaseSender();
        spout->CloseDirectX11();

        bool ok = spout->SetAdapter(adapterIndex);
        if (ok && spout->OpenDirectX11()) {
            char desc[256] = {};
            spout->GetAdapterName(spout->GetAdapter(), desc, 256);
            printf("[SPOUT DX] Successfully re-opened Direct3D 11 on Adapter [%d]: %s\n", spout->GetAdapter(), desc);

            spout->SetSenderName(g_senderName);
            spout->SetSenderFormat(DXGI_FORMAT_B8G8R8A8_UNORM);
            InitD3D11Renderer(g_renderWidth, g_renderHeight);
            return true;
        }

        printf("[SPOUT DX ERROR] Failed to switch to adapter [%d], reverting to default...\n", adapterIndex);
        spout->SetAdapter(0);
        spout->OpenDirectX11();
        spout->SetSenderName(g_senderName);
        spout->SetSenderFormat(DXGI_FORMAT_B8G8R8A8_UNORM);
        InitD3D11Renderer(g_renderWidth, g_renderHeight);
        return false;
    }

    __declspec(dllexport) int Spout_GetAdapter(void* handle) {
        if (!handle) return -1;
        spoutDX* spout = static_cast<spoutDX*>(handle);
        return spout->GetAdapter();
    }

    __declspec(dllexport) bool Spout_GetAdapterName(void* handle, char* buf, int maxChars) {
        if (!handle || !buf || maxChars <= 0) return false;
        spoutDX* spout = static_cast<spoutDX*>(handle);
        char desc[256] = {};
        spout->GetAdapterName(spout->GetAdapter(), desc, 256);
        strncpy_s(buf, maxChars, desc, maxChars - 1);
        return true;
    }

    __declspec(dllexport) bool Spout_GetSenderName(void* handle, char* buf, int maxChars) {
        if (!handle || !buf || maxChars <= 0) return false;
        spoutDX* spout = static_cast<spoutDX*>(handle);
        const char* name = spout->GetName();
        if (name) {
            strncpy_s(buf, maxChars, name, maxChars - 1);
            return true;
        }
        return false;
    }

    // Direct3D 11 GPU-Native Shader Rendering and Zero-Copy Spout Send
    __declspec(dllexport) bool Spout_RenderShaderFrame(void* handle, const RenderParams* params) {
        if (!handle || !params || !g_renderTargetView || !g_renderTargetTex) {
            return false;
        }
        spoutDX* spout = static_cast<spoutDX*>(handle);
        ID3D11DeviceContext* pContext = spout->GetDX11Context();
        if (!pContext) return false;

        // 1. Update Constant Buffer
        pContext->UpdateSubresource(g_constantBuffer, 0, nullptr, params, 0, 0);

        // 2. Set Pipeline States
        pContext->OMSetRenderTargets(1, &g_renderTargetView, nullptr);
        pContext->RSSetViewports(1, &g_viewport);
        pContext->RSSetState(g_rasterizerState);
        pContext->VSSetShader(g_vertexShader, nullptr, 0);
        pContext->PSSetShader(g_pixelShader, nullptr, 0);
        pContext->PSSetConstantBuffers(0, 1, &g_constantBuffer);
        pContext->IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);

        // 3. Draw Fullscreen Triangle (Execute GPU Pixel Shader on RTX 4060)
        pContext->Draw(3, 0);

        // 4. Send Rendered Texture to Spout2 (Zero-Copy VRAM pointer exchange!)
        return spout->SendTexture(g_renderTargetTex);
    }

    // Legacy CPU image sender (kept for backward compatibility)
    __declspec(dllexport) bool Spout_SendImage(
        void* handle,
        const unsigned char* pixels,
        unsigned int width,
        unsigned int height
    ) {
        if (!handle || !pixels || width == 0 || height == 0) {
            return false;
        }
        spoutDX* spout = static_cast<spoutDX*>(handle);
        return spout->SendImage(pixels, width, height, width * 4);
    }

    __declspec(dllexport) bool Spout_IsInitialized(void* handle) {
        if (!handle) return false;
        return static_cast<spoutDX*>(handle)->IsInitialized();
    }

    __declspec(dllexport) void Spout_Release(void* handle) {
        if (!handle) return;
        ReleaseD3D11Renderer();
        spoutDX* spout = static_cast<spoutDX*>(handle);
        spout->ReleaseSender();
        spout->CloseDirectX11();
        delete spout;
        if (g_spout == spout) g_spout = nullptr;
    }
}
