#include <windows.h>
#include <d3d11.h>
#include <stdio.h>
#include "SpoutDX.h"

extern "C" {
    __declspec(dllexport) void Spout_InspectAdapters() {
        spoutDX spout;
        int num = spout.GetNumAdapters();
        printf("Number of graphics adapters: %d\n", num);
        for (int i = 0; i < num; i++) {
            char name[256] = {};
            spout.GetAdapterName(i, name, 256);
            printf("  Adapter [%d]: %s\n", i, name);
        }
        printf("Current adapter index: %d (%s)\n", spout.GetAdapter(), spout.AdapterName());
    }
}
