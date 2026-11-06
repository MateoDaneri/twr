-- Archivo de prueba del prototipo. No contiene datos de proyectos reales.
local M = {}

function M.normalize(value)
  if value == nil then
    return ""
  end
  return tostring(value)
    :gsub("^%s+", "")
    :gsub("%s+$", "")
end

return M
