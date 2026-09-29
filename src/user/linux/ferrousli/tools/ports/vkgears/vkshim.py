#!/usr/bin/env python3
"""Write a static Vulkan loader for the calls a program makes.

    vkshim.py <vk.xml> <names> <out.c>

A Vulkan program links against a loader, which finds a driver on disk and
opens it with dlopen. Ferrix's ports are static and ferrousli's dlopen refuses
a library with thread-local storage, which Mesa has, so the Venus driver is
linked into the program instead (docs/GPU.md 6.1). What stands in for the
loader is this file's output: one function per Vulkan call the program leaves
undefined, each resolving itself once through the driver's
vk_icdGetInstanceProcAddr and then calling what it was given.

<names> is the program's undefined vk* symbols, one to a line, as `nm -u`
prints them. Their prototypes come from the Khronos registry, vk.xml, of the
same Vulkan-Headers release the program is compiled against.

Only the calls a loader answers without a driver's instance, and the one that
makes an instance, are special:

* vkGetInstanceProcAddr is the driver's own;
* vkCreateInstance and the vkEnumerateInstance* calls are resolved with no
  instance, as the Vulkan specification says a loader resolves them;
* vkCreateInstance remembers the instance it made, and every other call is
  resolved against it. Mesa hands back device-level calls from
  vkGetInstanceProcAddr as trampolines that dispatch on their first handle, so
  one instance is enough for a program with one device.
"""

import sys
import xml.etree.ElementTree as ET

GLOBAL = {
    "vkCreateInstance",
    "vkEnumerateInstanceExtensionProperties",
    "vkEnumerateInstanceLayerProperties",
    "vkEnumerateInstanceVersion",
}


def prototypes(registry):
    """Every command's (return type, name, [(declaration, name)]) by name."""
    commands = {}
    aliases = {}
    # Only the definitions: a <require> block names commands too.
    for command in registry.find("commands").findall("command"):
        if "vulkan" not in command.get("api", "vulkan").split(","):
            continue
        if command.get("alias"):
            aliases[command.get("name")] = command.get("alias")
            continue
        proto = command.find("proto")
        name = proto.find("name").text
        returns = "".join(proto.itertext())[: -len(name)].strip()
        params = []
        for param in command.findall("param"):
            api = param.get("api")
            if api and "vulkan" not in api.split(","):
                continue
            declaration = " ".join("".join(param.itertext()).split())
            params.append((declaration, param.find("name").text))
        commands[name] = (returns, params)
    for alias, target in aliases.items():
        commands[alias] = commands[target]
    return commands


def trampoline(name, returns, params):
    declared = ", ".join(declaration for declaration, _ in params) or "void"
    passed = ", ".join(argument for _, argument in params)
    call = f"fn({passed})"
    body = f"return {call};" if returns != "void" else f"{call};"
    if name == "vkCreateInstance":
        return (
            f"VKAPI_ATTR {returns} VKAPI_CALL {name}({declared})\n"
            "{\n"
            f"    PFN_{name} fn = (PFN_{name})vk_icdGetInstanceProcAddr(VK_NULL_HANDLE, \"{name}\");\n"
            f"    {returns} result = {call};\n"
            "    if (result == VK_SUCCESS)\n"
            "        vkshim_instance = *pInstance;\n"
            "    return result;\n"
            "}\n"
        )
    instance = "VK_NULL_HANDLE" if name in GLOBAL else "vkshim_instance"
    return (
        f"VKAPI_ATTR {returns} VKAPI_CALL {name}({declared})\n"
        "{\n"
        f"    static PFN_{name} fn;\n"
        "    if (!fn)\n"
        f"        fn = (PFN_{name})vkshim_resolve({instance}, \"{name}\");\n"
        f"    {body}\n"
        "}\n"
    )


HEADER = """\
/* Written by vkshim.py from vk.xml for the calls one program makes: a static
 * stand-in for the Vulkan loader, over a driver linked into the program.
 * Do not edit; run the port's build again. */
#define VK_USE_PLATFORM_WAYLAND_KHR 1
#include <stdio.h>
#include <stdlib.h>
#include <vulkan/vulkan.h>

VKAPI_ATTR PFN_vkVoidFunction VKAPI_CALL
vk_icdGetInstanceProcAddr(VkInstance instance, const char *name);
VKAPI_ATTR VkResult VKAPI_CALL
vk_icdNegotiateLoaderICDInterfaceVersion(uint32_t *version);

static VkInstance vkshim_instance;

static PFN_vkVoidFunction vkshim_resolve(VkInstance instance, const char *name)
{
    static int negotiated;
    if (!negotiated) {
        /* What a loader does first: the driver is told the version of the
         * loader interface it is spoken to in, the newest Mesa knows. */
        uint32_t version = 7;
        vk_icdNegotiateLoaderICDInterfaceVersion(&version);
        negotiated = 1;
    }
    PFN_vkVoidFunction fn = vk_icdGetInstanceProcAddr(instance, name);
    if (!fn) {
        fprintf(stderr, "vkshim: the driver has no %s\\n", name);
        abort();
    }
    return fn;
}

VKAPI_ATTR PFN_vkVoidFunction VKAPI_CALL
vkGetInstanceProcAddr(VkInstance instance, const char *name)
{
    return vk_icdGetInstanceProcAddr(instance, name);
}
"""


def main():
    registry_path, names_path, out_path = sys.argv[1:]
    commands = prototypes(ET.parse(registry_path).getroot())
    with open(names_path, encoding="utf-8") as names_file:
        names = sorted(
            {line.strip() for line in names_file if line.strip().startswith("vk")}
            - {"vkGetInstanceProcAddr"}
        )
    unknown = [name for name in names if name not in commands]
    if unknown:
        sys.exit(f"vkshim: not Vulkan calls in vk.xml: {' '.join(unknown)}")
    parts = [HEADER]
    parts += (trampoline(name, *commands[name]) for name in names)
    with open(out_path, "w", encoding="utf-8", newline="\n") as out:
        out.write("\n".join(parts))
    print(f"vkshim: {len(names)} calls")


if __name__ == "__main__":
    main()
