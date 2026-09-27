
local script_source = debug.getinfo(1, "S").source
local script_path = script_source:sub(1, 1) == "@" and script_source:sub(2) or script_source
local script_dir = script_path:match("^(.*)/[^/]+$") or "."

local function shell_quote(value)
    return "'" .. value:gsub("'", "'\"'\"'") .. "'"
end

local function path_join(a, b)
    if a:sub(-1) == "/" then return a .. b end
    return a .. "/" .. b
end

local function exists(path)
    local ok = os.rename(path, path)
    return ok ~= nil
end

local function run(command)
    local ok, why, code = os.execute(command)
    return ok == true or ok == 0 or code == 0, why, code
end

local function read_file(path)
    local file, err = io.open(path, "rb")
    assert(file, "failed to open " .. path .. ": " .. tostring(err))
    local data = file:read("*a")
    file:close()
    return data
end

local function write_file(path, data)
    local dir = path:match("(.+)/[^/]+$")
    if dir and not exists(dir) then
        local ok = run("mkdir -p " .. shell_quote(dir))
        assert(ok, "failed to create directory: " .. dir)
    end

    local file, err = io.open(path, "wb")
    assert(file, "failed to write " .. path .. ": " .. tostring(err))
    assert(file:write(data))
    assert(file:close())
end

local function recursive_find_files(dir)
    local pipe = assert(io.popen("find " .. shell_quote(dir) .. " -type f -print"))
    local list = {}
    for path in pipe:lines() do list[#list + 1] = path end
    local ok, why, code = pipe:close()
    assert(ok == true or ok == 0 or code == 0,
        "failed to enumerate SDK files: " .. tostring(why or code))
    return list
end

local function parse_version(version)
    local major, minor, patch = version:match("^(%d+)%.(%d+)%.(%d+)$")
    if not major then return nil end
    return tonumber(major), tonumber(minor), tonumber(patch)
end

local function find_version_in_cargo(path)
    local data = read_file(path)
    return data:match('version%s*=%s*"([^"]+)"')
end

local function escape_pattern(value)
    return value:gsub("([%(%)%.%%%+%-%*%?%[%]%^%$])", "%%%1")
end

local function replace_version_in_file(path, current_version, target_version)
    local data = read_file(path)
        if data:find("%z") then return false end
    local updated = data:gsub(escape_pattern(current_version), target_version)
    if updated ~= data then
        write_file(path, updated)
        return true
    end
    return false
end

local function replace_cargo_package_version(path, current_version, target_version)
    local data = read_file(path)
    local package_start = data:find("%[package%]")
    assert(package_start, "missing [package] section: " .. path)
    local section_end = data:find("\n%[", package_start + 1) or (#data + 1)
    local header = data:sub(1, package_start - 1)
    local section = data:sub(package_start, section_end - 1)
    local tail = data:sub(section_end)

    local pattern = '(version%s*=%s*)"' .. escape_pattern(current_version) .. '"'
    local updated_section, count = section:gsub(pattern, '%1"' .. target_version .. '"', 1)
    assert(count == 1, "[package] version not found or already updated: " .. path)

    write_file(path, header .. updated_section .. tail)
end

local function replace_sdk_path_references(path, current_version, target_version)
    local data = read_file(path)
    local pattern = "sdk/neoutl/" .. escape_pattern(current_version) .. "/"
    local replacement = "sdk/neoutl/" .. target_version .. "/"
    local updated, count = data:gsub(pattern, replacement)
    if count > 0 then
        write_file(path, updated)
    end
end

local function copy_dir(source, target)
    local ok = run("mkdir -p " .. shell_quote(target))
    assert(ok, "failed to create target SDK directory: " .. target)
    ok = run("cp -a " .. shell_quote(source) .. "/. " .. shell_quote(target) .. "/")
    if not ok then
        run("rm -rf " .. shell_quote(target))
        error("failed to copy SDK directory")
    end
end

local function check_c_header_version(path, major, minor, patch, version)
    local data = read_file(path)
    local expected = {
        NEOUTL_SDK_VERSION_MAJOR = major,
        NEOUTL_SDK_VERSION_MINOR = minor,
        NEOUTL_SDK_VERSION_PATCH = patch,
    }
    for name, value in pairs(expected) do
        local actual = data:match("#define%s+" .. name .. "%s+(%d+)")
        assert(tonumber(actual) == value, "source C SDK header has an inconsistent " .. name)
    end
    assert(data:find('#define NEOUTL_SDK_VERSION_STRING "' .. version .. '"', 1, true),
        "source C SDK header version string does not match Cargo.toml")
end

local function set_c_header_version(path, major, minor, patch, version)
    local data = read_file(path)
    local function replace_macro(name, value)
        local pattern = "(#define%s+" .. name .. "%s+)%d+"
        local count
        data, count = data:gsub(pattern, "%1" .. tostring(value), 1)
        assert(count == 1, "missing or duplicate SDK version macro: " .. name)
    end
    replace_macro("NEOUTL_SDK_VERSION_MAJOR", major)
    replace_macro("NEOUTL_SDK_VERSION_MINOR", minor)
    replace_macro("NEOUTL_SDK_VERSION_PATCH", patch)
    local updated, count = data:gsub(
        '(#define%s+NEOUTL_SDK_VERSION_STRING%s+)"[^"]+"',
        '%1"' .. version .. '"', 1)
    assert(count == 1, "missing NEOUTL_SDK_VERSION_STRING")
    write_file(path, updated)
end

local args = {}
for _, arg in ipairs(arg) do args[#args + 1] = arg end

local target_version
for _, value in ipairs(args) do
    if value == "-h" or value == "--help" then
        print("usage: luajit update.lua 0.9.0")
        print("       luajit update.lua --0.9.0")
        os.exit(0)
    end

    local candidate = value:gsub("^%-%-?", ""):gsub("^v", "")
    if not parse_version(candidate) then
        io.stderr:write("invalid argument/version: " .. value .. "\n")
        os.exit(1)
    end
    if target_version then
        io.stderr:write("provide exactly one target version\n")
        os.exit(1)
    end
    target_version = candidate
end

if not target_version then
    io.stderr:write("version is required, e.g. luajit update.lua 0.9.0\n")
    os.exit(1)
end

local major, minor, patch = parse_version(target_version)
local cargo_path = path_join(script_dir, "Cargo.toml")
local current_version = find_version_in_cargo(cargo_path)
if not current_version then
    io.stderr:write("failed to detect current version from Cargo.toml\n")
    os.exit(1)
end

local source_dir = path_join(path_join(script_dir, "sdk/neoutl"), current_version)
local target_dir = path_join(path_join(script_dir, "sdk/neoutl"), target_version)
local sdk_index = path_join(script_dir, "sdk/README.md")
local c_master_header = path_join(target_dir, "c/include/neoutl/neoutl.h")

if not parse_version(current_version) then
    io.stderr:write("Cargo.toml contains an invalid semantic version: " .. current_version .. "\n")
    os.exit(1)
end
if not exists(source_dir) then
    io.stderr:write("source SDK directory does not exist: " .. source_dir .. "\n")
    os.exit(1)
end
if find_version_in_cargo(path_join(source_dir, "rust/Cargo.toml")) ~= current_version then
    io.stderr:write("source SDK version does not match root Cargo.toml: " .. source_dir .. "\n")
    os.exit(1)
end
local source_major, source_minor, source_patch = parse_version(current_version)
check_c_header_version(
    path_join(source_dir, "c/include/neoutl/neoutl.h"),
    source_major, source_minor, source_patch, current_version)
if exists(target_dir) then
    io.stderr:write("target SDK directory already exists: " .. target_dir .. "\n")
    io.stderr:write("remove it first or choose another version\n")
    os.exit(1)
end
if not exists(sdk_index) then
    io.stderr:write("SDK index README does not exist: " .. sdk_index .. "\n")
    os.exit(1)
end

print("1/4 source version: " .. current_version)
print("2/4 copying SDK: " .. source_dir .. " -> " .. target_dir)
copy_dir(source_dir, target_dir)

print("3/4 updating project, SDK docs, Cargo and C header versions")
local files = recursive_find_files(target_dir)
for _, file in ipairs(files) do
    replace_version_in_file(file, current_version, target_version)
end
replace_cargo_package_version(cargo_path, current_version, target_version)
replace_sdk_path_references(cargo_path, current_version, target_version)
replace_version_in_file(sdk_index, current_version, target_version)
set_c_header_version(c_master_header, major, minor, patch, target_version)

local sdk_cargo = path_join(target_dir, "rust/Cargo.toml")
assert(find_version_in_cargo(sdk_cargo) == target_version,
    "generated Rust SDK Cargo.toml has an unexpected version")
assert(find_version_in_cargo(cargo_path) == target_version,
    "root Cargo.toml was not updated")
local generated_header = read_file(c_master_header)
assert(generated_header:find('#define NEOUTL_SDK_VERSION_STRING "' .. target_version .. '"', 1, true),
    "C SDK header version string was not updated")
local generated_index = read_file(sdk_index)
assert(generated_index:find("neoutl/" .. target_version .. "/rust/README.md", 1, true),
    "SDK index does not link to the generated version")

print("4/4 done")
print("updated SDK path: sdk/neoutl/" .. target_version)
print("updated SDK index: sdk/README.md")
